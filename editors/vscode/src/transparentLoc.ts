// Transparent localisation read/write for EU4dll-transcoded files.
//
// The `pdcloc://` scheme mirrors a `file://` URI over the same real path and
// is the one view the user edits: readFile shows readable CJK everywhere
// (legacy whole-escaped files decode wholesale; scoped files decode only
// their quoted strings, keeping comments readable), and writeFile
// scoped-encodes on save — escape triples land inside quoted strings only, so
// the game-side transcoder reads the strings exactly as with whole-file
// encoding while comments and code stay readable UTF-8 on disk. The language
// server receives the decoded text through normal document sync (it resolves
// `pdcloc://` URIs to the backing path, so the virtual document hides the
// on-disk shard and every position is a decoded-view position — no offset
// mapping).
//
// Entry is path-based: every eligible file (localisation yml or the
// configured script globs) opens through its pdcloc:// twin, whatever its
// bytes look like. Plain readable files pass through unchanged (a save
// encodes their quoted CJK), damaged files pass through with an error, and
// only the classifier decides which decode applies — never the caller.
//
// Iron rule ② (never encode twice / decode twice) is enforced in both
// directions: the scoped decoder resolves triples only inside strings, and a
// save is refused outright when a string already contains escape markers —
// with the reason surfaced to the user.
//
// Opening an eligible file takes over its tab instead of adding a second
// one: the decoded view is shown in the raw tab's own slot (preview
// state included) and the raw tab is closed. `revealOriginal` pins the raw
// form the same way, and `peekOriginal` flips the slot momentarily — Esc or
// an editor-group switch flips it back (escape triples never contain newline
// bytes, so the cursor line survives every flip).

import * as fs from 'node:fs/promises';
import * as nodePath from 'node:path';
import * as vscode from 'vscode';
import { globToRegExp } from './paths';
import {
    Transcoder,
    PROFILE_LOCALISATION,
    PROFILE_SCRIPT,
    isRawCjk,
    markerOutsideSpans,
    scanStringSpans,
    type LocalisationProfile,
    type ScopedEncodeResult,
    type ScopedForm,
} from './transcode';

export const PDCLOC_SCHEME = 'pdcloc';

/** The `pdcloc://` twin of a real `file://` URI. */
export function decodedUriOf(real: vscode.Uri): vscode.Uri {
    return real.with({ scheme: PDCLOC_SCHEME });
}

/** The real `file://` URI behind a `pdcloc://` view, if any. */
export function realUriOf(uri: vscode.Uri): vscode.Uri | undefined {
    if (uri.scheme !== PDCLOC_SCHEME) {
        return undefined;
    }
    return uri.with({ scheme: 'file' });
}

/**
 * Selects the transcoder profile for a real file: localisation yml under a
 * `localisation/` directory uses the UTF-8/BOM form (paratranz `utf8eu4`);
 * files matching the configured script globs (workspace-relative) use the raw
 * single-byte form (`latin1eu4`). Anything else is not eligible for the
 * transparent view at all.
 */
export function profileForRealPath(
    fsPath: string,
    scriptGlobs: string[],
): LocalisationProfile | undefined {
    const normalized = fsPath.replace(/\\/g, '/');
    if (/\/localisation\//i.test(normalized) && /\.yml$/i.test(normalized)) {
        return PROFILE_LOCALISATION;
    }
    const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(fsPath));
    if (!folder) {
        return undefined;
    }
    const relative = nodePath
        .relative(folder.uri.fsPath, fsPath)
        .replace(/\\/g, '/');
    if (relative.startsWith('..')) {
        return undefined;
    }
    if (scriptGlobs.some((glob) => globToRegExp(glob).test(relative))) {
        return PROFILE_SCRIPT;
    }
    return undefined;
}

function transparentScriptGlobs(): string[] {
    return vscode.workspace
        .getConfiguration('paradoxcode.localisation')
        .get<string[]>('transparentScriptGlobs', ['**/*.txt'])
        .filter((value): value is string => typeof value === 'string' && value.length > 0);
}

function formatCodePoint(codePoint: number): string {
    return `U+${codePoint.toString(16).toUpperCase().padStart(4, '0')}`;
}

const fatalUtf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

/** Zero-based line number of a byte offset, from the passthrough bytes. */
function lineAt(bytes: Uint8Array, offset: number): number {
    let lines = 0;
    for (let index = 0; index < offset && index < bytes.length; index += 1) {
        if (bytes[index] === 0x0a) {
            lines += 1;
        }
    }
    return lines;
}

/** Whether any quoted string holds raw CJK — the content a plain-form save encodes. */
function hasQuotedCjk(bytes: Uint8Array): boolean {
    try {
        fatalUtf8.decode(bytes);
    } catch {
        return false; // CP1252-only bytes cannot hold CJK
    }
    for (const [start, end] of scanStringSpans(bytes)) {
        const span = fatalUtf8.decode(bytes.subarray(start, end));
        for (let index = 0; index < span.length; ) {
            const codePoint = span.codePointAt(index) as number;
            if (isRawCjk(codePoint)) {
                return true;
            }
            index += codePoint > 0xffff ? 2 : 1;
        }
    }
    return false;
}

/** What readFile observed for a URI — consumed by the diagnostics publisher. */
interface ReadOutcome {
    form: ScopedForm;
    /** Orphan markers the decoder passed through (whole-file or in-span). */
    broken: number;
    /** First input byte offset of damage, for anchoring the damaged-form error. */
    damagedAt: number | undefined;
}

class PdclocFileSystemProvider implements vscode.FileSystemProvider {
    private readonly emitter = new vscode.EventEmitter<vscode.FileChangeEvent[]>();
    private readonly watchers = new Set<vscode.FileSystemWatcher>();

    constructor(
        private readonly transcoder: Transcoder,
        private readonly diagnostics: vscode.DiagnosticCollection,
        private readonly log: vscode.OutputChannel,
    ) {}

    readonly onDidChangeFile = this.emitter.event;

    dispose(): void {
        this.emitter.dispose();
        for (const watcher of this.watchers) {
            watcher.dispose();
        }
        this.watchers.clear();
    }

    watch(uri: vscode.Uri): vscode.Disposable {
        const real = realUriOf(uri);
        if (!real) {
            return new vscode.Disposable(() => undefined);
        }
        // External disk changes must re-enter through readFile so the decoded
        // view picks up fresh transcodes without a manual reload.
        const watcher = vscode.workspace.createFileSystemWatcher(real.fsPath);
        this.watchers.add(watcher);
        watcher.onDidChange(() =>
            this.emitter.fire([{ uri, type: vscode.FileChangeType.Changed }]),
        );
        watcher.onDidDelete(() =>
            this.emitter.fire([{ uri, type: vscode.FileChangeType.Deleted }]),
        );
        return new vscode.Disposable(() => {
            watcher.dispose();
            this.watchers.delete(watcher);
        });
    }

    async stat(uri: vscode.Uri): Promise<vscode.FileStat> {
        const real = this.requireReal(uri);
        const stats = await fs.stat(real.fsPath);
        return {
            type: stats.isDirectory() ? vscode.FileType.Directory : vscode.FileType.File,
            ctime: stats.ctimeMs,
            mtime: stats.mtimeMs,
            size: stats.size,
        };
    }

    async readFile(uri: vscode.Uri): Promise<Uint8Array> {
        const real = this.requireReal(uri);
        const profile = this.requireProfile(real);
        const bytes = new Uint8Array(await fs.readFile(real.fsPath));
        const form = this.transcoder.scopedClassify(bytes, profile);
        if (form === 'plain' || form === 'damaged') {
            // Plain bytes pass through unchanged — a save encodes their quoted
            // CJK. Damaged content is never transformed, only reported.
            this.publishReadDiagnostics(uri, real, bytes, {
                form,
                broken: 0,
                damagedAt: form === 'damaged' ? markerOutsideSpans(bytes)[0] : undefined,
            });
            return bytes;
        }
        if (form === 'whole') {
            const decoded = this.transcoder.decode(bytes, profile);
            if (decoded === 'invalid-utf8') {
                throw vscode.FileSystemError.Unavailable(
                    'an escaped localisation file must be valid UTF-8 — the bytes are damaged',
                );
            }
            this.publishReadDiagnostics(uri, real, bytes, { form, broken: decoded.broken, damagedAt: undefined });
            return decoded.text;
        }
        const decoded = this.transcoder.scopedDecode(bytes, profile);
        if (decoded === 'invalid-utf8') {
            throw vscode.FileSystemError.Unavailable(
                'an escaped localisation file must be valid UTF-8 — the bytes are damaged',
            );
        }
        this.publishReadDiagnostics(uri, real, bytes, {
            form,
            broken: decoded.inSpanBroken.length,
            damagedAt: undefined,
        });
        return decoded.text;
    }

    async writeFile(
        uri: vscode.Uri,
        content: Uint8Array,
        _options?: { create: boolean; overwrite: boolean },
    ): Promise<void> {
        const real = this.requireReal(uri);
        const profile = this.requireProfile(real);
        let encoded: ScopedEncodeResult;
        try {
            encoded = this.transcoder.scopedEncode(content, profile);
        } catch (error) {
            const message = 'Refused to save: the editor buffer is not valid UTF-8.';
            this.log.appendLine(
                `transparentLoc: refused save of ${real.fsPath} (${error instanceof Error ? error.message : String(error)})`,
            );
            void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
            throw vscode.FileSystemError.NoPermissions(message);
        }
        if ('alreadyEscaped' in encoded) {
            const message =
                `Refused to save: ${encoded.alreadyEscaped.length} escape marker(s) already sit inside ` +
                'quoted strings — encoding them again would double-encode. Undo the paste ' +
                '(or decode the text) first.';
            this.log.appendLine(
                `transparentLoc: refused save of ${real.fsPath} (in-span escape markers)`,
            );
            void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
            throw vscode.FileSystemError.NoPermissions(message);
        }
        if ('unencodable' in encoded) {
            const points = encoded.unencodable
                .map((point) => formatCodePoint(point.codePoint))
                .slice(0, 8)
                .join(', ');
            const message =
                `Refused to save: ${encoded.unencodable.length} code point(s) cannot be round-tripped ` +
                `by the EU4 transcoder (${points}). Replace or remove them first.`;
            this.log.appendLine(`transparentLoc: refused save of ${real.fsPath} (${message})`);
            void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
            throw vscode.FileSystemError.NoPermissions(message);
        }
        await fs.writeFile(real.fsPath, encoded.bytes);
        this.diagnostics.delete(uri);
        this.diagnostics.delete(real);
    }

    async readDirectory(): Promise<[string, vscode.FileType][]> {
        throw vscode.FileSystemError.NoPermissions('pdcloc:// exposes individual files only');
    }

    async createDirectory(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('pdcloc:// exposes individual files only');
    }

    async delete(uri: vscode.Uri): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(
            `delete is not supported for pdcloc:// views (${realUriOf(uri)?.fsPath ?? uri.toString()})`,
        );
    }

    async rename(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('rename is not supported for pdcloc:// views');
    }

    async copy(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('copy is not supported for pdcloc:// views');
    }

    private requireReal(uri: vscode.Uri): vscode.Uri {
        const real = realUriOf(uri);
        if (!real) {
            throw vscode.FileSystemError.FileNotFound(
                `${PDCLOC_SCHEME}:// requires a real backing path`,
            );
        }
        return real;
    }

    private requireProfile(real: vscode.Uri): LocalisationProfile {
        const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
        if (profile === undefined) {
            throw vscode.FileSystemError.Unavailable(
                'this file is not eligible for the transparent localisation view ' +
                    '(localisation yml or the configured script globs)',
            );
        }
        return profile;
    }

    private publishReadDiagnostics(
        uri: vscode.Uri,
        real: vscode.Uri,
        bytes: Uint8Array,
        outcome: ReadOutcome,
    ): void {
        let diagnostic: vscode.Diagnostic | undefined;
        if (outcome.form === 'damaged') {
            const line = outcome.damagedAt === undefined ? 0 : lineAt(bytes, outcome.damagedAt);
            diagnostic = new vscode.Diagnostic(
                new vscode.Range(line, 0, line, 0),
                'Stray EU4dll escape marker(s) outside every quoted string — the file is damaged ' +
                    'and shown as-is. Move them inside a string or remove them.',
                vscode.DiagnosticSeverity.Error,
            );
            diagnostic.code = 'LocalisationMixedEncoding';
        } else if (outcome.form === 'plain' && hasQuotedCjk(bytes)) {
            diagnostic = new vscode.Diagnostic(
                new vscode.Range(0, 0, 0, 0),
                'Quoted CJK text stays readable here; saving encodes it into EU4dll escape ' +
                    'triples inside the strings (comments and code stay readable on disk).',
                vscode.DiagnosticSeverity.Information,
            );
            diagnostic.code = 'LocalisationWillTranscodeOnSave';
        } else if (outcome.broken > 0) {
            diagnostic = new vscode.Diagnostic(
                new vscode.Range(0, 0, 0, 0),
                `${outcome.broken} orphan escape marker(s) were passed through undecoded — check for damaged triples.`,
                vscode.DiagnosticSeverity.Warning,
            );
            diagnostic.code = 'LocalisationBrokenEscapeSequence';
        }
        if (diagnostic) {
            diagnostic.source = 'paradoxcode';
            this.diagnostics.set(uri, [diagnostic]);
            this.diagnostics.set(real, [diagnostic]);
        } else {
            this.diagnostics.delete(uri);
            this.diagnostics.delete(real);
        }
    }
}

/** Command argument resolution: palette → active editor, menus → resource. */
function commandResource(uri: vscode.Uri | undefined): vscode.Uri | undefined {
    if (uri instanceof vscode.Uri) {
        return uri;
    }
    const active = vscode.window.activeTextEditor?.document.uri;
    if (!active) {
        return undefined;
    }
    return realUriOf(active) ?? active;
}

/**
 * Raw URIs intentionally kept on their `file://` view — pinned by
 * revealOriginal or an active peek. The automatic redirect skips them until
 * their tab closes (pruned from the tab-change listener below).
 */
const manualRawViews = new Set<string>();

interface TextTabSlot {
    tab: vscode.Tab;
    group: vscode.TabGroup;
}

/** Plain text tabs showing `uri` — diff, custom, and notebook inputs are excluded on purpose. */
function textTabsFor(uri: vscode.Uri): TextTabSlot[] {
    const slots: TextTabSlot[] = [];
    for (const group of vscode.window.tabGroups.all) {
        for (const tab of group.tabs) {
            if (tab.input instanceof vscode.TabInputText && tab.input.uri.toString() === uri.toString()) {
                slots.push({ tab, group });
            }
        }
    }
    return slots;
}

async function closeTextTabs(uri: vscode.Uri): Promise<void> {
    const tabs = textTabsFor(uri).map((slot) => slot.tab);
    if (tabs.length > 0) {
        await vscode.window.tabGroups.close(tabs);
    }
}

/**
 * Shows `target` in `source`'s tab slot — same group, same preview state —
 * then closes whatever plain text tabs still show `source`. A preview slot is
 * reused in place; a pinned tab is replaced beside itself, so the swap never
 * leaves a second tab behind. When `source` has no tab yet the target simply
 * opens in the active group as a persistent tab.
 */
async function takeoverShow(source: vscode.Uri, target: vscode.Uri): Promise<vscode.TextEditor> {
    const slot = textTabsFor(source)[0];
    const editor = await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(target), {
        viewColumn: slot?.group.viewColumn,
        preview: slot?.tab.isPreview ?? false,
    });
    await closeTextTabs(source);
    return editor;
}

function currentLine(editor: vscode.TextEditor | undefined): number | undefined {
    return editor?.selection.active.line;
}

/**
 * Restores the cursor to `line` (column 0) after a flip: escape triples never
 * contain newline bytes, so line numbers map one-to-one between the raw and
 * decoded forms — only the column has to reset.
 */
function restoreLine(editor: vscode.TextEditor | undefined, line: number | undefined): void {
    if (!editor || line === undefined) {
        return;
    }
    const clamped = Math.min(Math.max(line, 0), editor.document.lineCount - 1);
    const position = new vscode.Position(clamped, 0);
    editor.selection = new vscode.Selection(position, position);
    editor.revealRange(editor.document.lineAt(clamped).range, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
}

async function openDecodedView(uri: vscode.Uri | undefined): Promise<void> {
    const real = commandResource(uri);
    if (!real || real.scheme !== 'file') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: open a localisation yml (or a configured script file) first.',
        );
        return;
    }
    if (profileForRealPath(real.fsPath, transparentScriptGlobs()) === undefined) {
        void vscode.window.showErrorMessage(
            'ParadoxCode: this file is not eligible for the decoded view ' +
                '(localisation/**/*.yml or paradoxcode.localisation.transparentScriptGlobs).',
        );
        return;
    }
    // Eligibility is the only gate: readFile dispatches on the actual form —
    // plain files pass through byte-identical, escaped and scoped files
    // decode, damaged files show as-is with an error.
    await takeoverShow(real, decodedUriOf(real));
    supersedePeek(real);
}

/** Pins the raw on-disk form: flips the decoded tab in place and keeps the raw view immune to the auto-redirect. */
async function revealOriginal(): Promise<void> {
    const active = vscode.window.activeTextEditor;
    const real = active ? realUriOf(active.document.uri) : undefined;
    if (!active || !real) {
        return;
    }
    const line = currentLine(active);
    manualRawViews.add(real.toString());
    const editor = await takeoverShow(active.document.uri, real);
    restoreLine(editor, line);
}

/** The active momentary raw view, if any. */
interface PeekState {
    real: vscode.Uri;
    decoded: vscode.Uri;
    column: vscode.ViewColumn | undefined;
}

const PEEKING_RAW_CONTEXT = 'paradoxcode.peekingRaw';

let peek: PeekState | undefined;

/** Drops the peek bookkeeping after its file got the decoded view back another way (e.g. the eye command). */
function supersedePeek(real: vscode.Uri): void {
    if (!peek || peek.real.toString() !== real.toString()) {
        return;
    }
    peek = undefined;
    manualRawViews.delete(real.toString());
    void vscode.commands.executeCommand('setContext', PEEKING_RAW_CONTEXT, false);
}

/**
 * Momentary look at the on-disk escaped form: the decoded tab flips in place
 * and Esc (or the eye command on the raw view) flips it back. Moving focus to
 * another editor group ends the peek in the background; a tab switch inside
 * the peek's own group cannot (VS Code has no API to rewrite a background
 * tab), so the peek simply survives there until Esc. Unsaved raw edits also
 * keep it open — refusing to yank the view is safer than losing the user's
 * place, and it degrades to the pinned revealOriginal mode.
 */
async function peekOriginal(): Promise<void> {
    const active = vscode.window.activeTextEditor;
    const decoded = active?.document.uri;
    const real = decoded ? realUriOf(decoded) : undefined;
    if (!active || !decoded || !real) {
        return;
    }
    if (peek) {
        await endPeek();
    }
    const line = currentLine(active);
    manualRawViews.add(real.toString());
    const editor = await takeoverShow(decoded, real);
    restoreLine(editor, line);
    peek = { real, decoded, column: editor.viewColumn };
    await vscode.commands.executeCommand('setContext', PEEKING_RAW_CONTEXT, true);
}

async function endPeek(foreground = true): Promise<void> {
    const state = peek;
    if (!state) {
        return;
    }
    const rawEditor = vscode.window.visibleTextEditors.find(
        (editor) => editor.document.uri.toString() === state.real.toString(),
    );
    if (rawEditor?.document.isDirty) {
        void vscode.window.setStatusBarMessage(
            'ParadoxCode: raw view kept open — it has unsaved edits (Esc again after saving or discarding them).',
            6000,
        );
        return;
    }
    peek = undefined;
    manualRawViews.delete(state.real.toString());
    void vscode.commands.executeCommand('setContext', PEEKING_RAW_CONTEXT, false);
    if (!rawEditor) {
        // The raw tab was closed outright (middle-click): nothing to flip back.
        return;
    }
    const slot = textTabsFor(state.real)[0];
    const line = currentLine(rawEditor);
    const editor = await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(state.decoded), {
        viewColumn: slot?.group.viewColumn,
        preview: slot?.tab.isPreview ?? false,
        preserveFocus: !foreground,
    });
    await closeTextTabs(state.real);
    restoreLine(editor, line);
}

/** Whether the transparent pipeline (pdcloc:// views, automatic redirection) is enabled. */
function transparentEncodingEnabled(): boolean {
    return vscode.workspace
        .getConfiguration('paradoxcode.localisation')
        .get<boolean>('transparentEncoding', true);
}

/** Whether an open editor — raw or decoded view — holds unsaved changes for `real`. */
function hasDirtyDocument(real: vscode.Uri): boolean {
    const raw = real.toString();
    const twin = decodedUriOf(real).toString();
    return vscode.workspace.textDocuments.some(
        (document) =>
            document.isDirty && (document.uri.toString() === raw || document.uri.toString() === twin),
    );
}

/** Shared guards for the manual one-shot commands: they need a target, a disabled
 * transparent pipeline (the master switch), an in-scope path, and no unsaved
 * editor for that file — both commands rewrite the file on disk. */
async function manualCommandTarget(
    verb: string,
    uri: vscode.Uri | undefined,
): Promise<vscode.Uri | undefined> {
    const real = commandResource(uri);
    if (!real || real.scheme !== 'file') {
        void vscode.window.showErrorMessage(`ParadoxCode: select a file to ${verb} first.`);
        return undefined;
    }
    if (transparentEncodingEnabled()) {
        void vscode.window.showInformationMessage(
            'ParadoxCode: transparent encoding is on — saves already encode automatically. ' +
                'Turn paradoxcode.localisation.transparentEncoding off to transcode by hand.',
        );
        return undefined;
    }
    if (profileForRealPath(real.fsPath, transparentScriptGlobs()) === undefined) {
        void vscode.window.showErrorMessage(
            'ParadoxCode: this file is not in the transcoding scope ' +
                '(localisation/**/*.yml or paradoxcode.localisation.transparentScriptGlobs).',
        );
        return undefined;
    }
    if (hasDirtyDocument(real)) {
        void vscode.window.showErrorMessage(
            `ParadoxCode: save or close the editor for this file first — ${verb}ing rewrites it on disk.`,
        );
        return undefined;
    }
    return real;
}

/**
 * Manual one-shot encode, the counterpart of the automatic pipeline: a
 * readable eligible file is rewritten in the scoped escaped form — triples
 * inside quoted strings only, comments and code as readable UTF-8. The
 * original is backed up next to the file first.
 */
async function encodeFileManually(
    transcoder: Transcoder,
    uri: vscode.Uri | undefined,
    log: vscode.OutputChannel,
): Promise<void> {
    const real = await manualCommandTarget('encode', uri);
    if (!real) {
        return;
    }
    const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        return;
    }
    const bytes = new Uint8Array(await fs.readFile(real.fsPath));
    const form = transcoder.scopedClassify(bytes, profile);
    if (form === 'whole' || form === 'scoped') {
        void vscode.window.showInformationMessage('ParadoxCode: file is already encoded.');
        return;
    }
    if (form === 'damaged') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: stray escape marker(s) outside every quoted string — fix the file manually first.',
        );
        return;
    }
    let encoded: ScopedEncodeResult;
    try {
        encoded = transcoder.scopedEncode(bytes, profile);
    } catch {
        void vscode.window.showErrorMessage(
            'ParadoxCode: refused to encode — the file is not valid UTF-8 (convert it first).',
        );
        return;
    }
    if ('alreadyEscaped' in encoded) {
        void vscode.window.showErrorMessage(
            `ParadoxCode: refused to encode — ${encoded.alreadyEscaped.length} escape marker(s) already sit inside quoted strings.`,
        );
        return;
    }
    if ('unencodable' in encoded) {
        const points = encoded.unencodable
            .map((point) => formatCodePoint(point.codePoint))
            .slice(0, 8)
            .join(', ');
        void vscode.window.showErrorMessage(
            `ParadoxCode: refused to encode — ${encoded.unencodable.length} unencodable code point(s) inside strings (${points}).`,
        );
        return;
    }
    const backup = `${real.fsPath}.pre-transcode.bak`;
    await fs.copyFile(real.fsPath, backup);
    await fs.writeFile(real.fsPath, encoded.bytes);
    log.appendLine(`transparentLoc: encoded ${real.fsPath} (backup: ${backup})`);
    void vscode.window.showInformationMessage(
        `ParadoxCode: encoded ${nodePath.basename(real.fsPath)} in the scoped escape form (backup: ${nodePath.basename(backup)}).`,
    );
}

/**
 * Manual one-shot decode: an escaped eligible file — legacy whole-file or
 * scoped form — is rewritten as readable UTF-8, with the same side-of-file
 * backup. Plain and damaged files are refused with the reason instead of
 * rewritten.
 */
async function decodeFileManually(
    transcoder: Transcoder,
    uri: vscode.Uri | undefined,
    log: vscode.OutputChannel,
): Promise<void> {
    const real = await manualCommandTarget('decode', uri);
    if (!real) {
        return;
    }
    const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        return;
    }
    const bytes = new Uint8Array(await fs.readFile(real.fsPath));
    const form = transcoder.scopedClassify(bytes, profile);
    if (form === 'plain') {
        void vscode.window.showInformationMessage('ParadoxCode: file is already readable.');
        return;
    }
    if (form === 'damaged') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: stray escape marker(s) outside every quoted string — fix the file manually first.',
        );
        return;
    }
    let text: Uint8Array;
    let orphans: number;
    if (form === 'whole') {
        const decoded = transcoder.decode(bytes, profile);
        if (decoded === 'invalid-utf8') {
            void vscode.window.showErrorMessage(
                'ParadoxCode: an escaped localisation file must be valid UTF-8 — the bytes are damaged.',
            );
            return;
        }
        text = decoded.text;
        orphans = decoded.broken;
    } else {
        const decoded = transcoder.scopedDecode(bytes, profile);
        if (decoded === 'invalid-utf8') {
            void vscode.window.showErrorMessage(
                'ParadoxCode: an escaped localisation file must be valid UTF-8 — the bytes are damaged.',
            );
            return;
        }
        text = decoded.text;
        orphans = decoded.inSpanBroken.length;
    }
    const backup = `${real.fsPath}.pre-transcode.bak`;
    await fs.copyFile(real.fsPath, backup);
    await fs.writeFile(real.fsPath, text);
    log.appendLine(`transparentLoc: decoded ${real.fsPath} (backup: ${backup})`);
    const note =
        orphans > 0
            ? ` — ${orphans} orphan marker(s) passed through undecoded, check damaged triples`
            : '';
    void vscode.window.showInformationMessage(
        `ParadoxCode: decoded ${nodePath.basename(real.fsPath)} to readable text (backup: ${nodePath.basename(backup)})${note}.`,
    );
}

const TRANSCODE_ELIGIBLE_CONTEXT = 'paradoxcode.transcodeEligible';

/**
 * Publishes `paradoxcode.transcodeEligible` for a document: true exactly when
 * its real path is in the transparent-encoding scope (localisation yml or the
 * configured script globs). A pure path check with no disk access — the same
 * gate the automatic takeover enforces, precomputed so menu `when` clauses
 * reveal the eye icon instead of failing on click.
 */
async function updateDecodedEntryContext(
    document: vscode.TextDocument | undefined,
): Promise<void> {
    const eligible =
        document !== undefined &&
        document.uri.scheme === 'file' &&
        profileForRealPath(document.uri.fsPath, transparentScriptGlobs()) !== undefined;
    await vscode.commands.executeCommand('setContext', TRANSCODE_ELIGIBLE_CONTEXT, eligible);
}

/**
 * Opens the decoded view when an eligible file is opened through its raw
 * path, taking over the raw tab instead of adding a second one. Eligibility
 * is path-based (no disk read): plain files pass through their pdcloc twin
 * unchanged, whole-escaped and scoped files decode, and damaged files show
 * as-is with an error — the form dispatch lives in readFile, never here.
 * This also adopts files the moment they land on an eligible path (Save As
 * from an untitled buffer, a file moved into scope), closing the typed-CJK
 * gap: their next save goes through the scoped encoder.
 *
 * The `opening` set is only an in-flight guard. URIs deliberately pinned on
 * their raw view (revealOriginal, an active peek) are skipped until their tab
 * closes; a raw document can still be intentionally revealed later, and
 * closing/reopening it should apply the setting again.
 */
async function maybeAutoOpenDecodedView(
    document: vscode.TextDocument,
    opening: Set<string>,
): Promise<void> {
    if (document.uri.scheme !== 'file') {
        return;
    }
    const key = document.uri.toString();
    if (opening.has(key) || manualRawViews.has(key)) {
        return;
    }
    if (profileForRealPath(document.uri.fsPath, transparentScriptGlobs()) === undefined) {
        return;
    }
    opening.add(key);
    try {
        // Only take over the editor the user is actually looking at: invisible
        // programmatic opens and quick tab switches must not pop a decoded tab.
        if (vscode.window.activeTextEditor?.document.uri.toString() !== key) {
            return;
        }
        await openDecodedView(document.uri);
    } finally {
        opening.delete(key);
    }
}

/**
 * Registers the transparent-localisation feature. `transparentEncoding` is
 * the master switch: on, the `pdcloc://` FileSystemProvider, decoded-view
 * commands, automatic path-based redirection, the status-bar indicator, and
 * save-time scoped encoding are all active; off, only the manual one-shot
 * encode/decode commands remain (their menus hide through the same config
 * check). When the artifact is missing the feature degrades to a log line,
 * never an error.
 */
export async function activateTransparentLocalisation(
    context: vscode.ExtensionContext,
    log: vscode.OutputChannel,
): Promise<vscode.Disposable> {
    const transcoder = new Transcoder();

    const diagnostics = vscode.languages.createDiagnosticCollection('paradoxcode.transparent');
    const provider = new PdclocFileSystemProvider(transcoder, diagnostics, log);

    const statusItem = vscode.window.createStatusBarItem(
        'paradoxcode.transparentLoc',
        vscode.StatusBarAlignment.Left,
        99,
    );
    statusItem.name = 'ParadoxCode Decoded View';
    statusItem.text = '$(eye) EU4 decoded view';
    statusItem.command = 'paradoxcode.localisation.revealOriginal';

    const updateStatus = (): void => {
        const active = vscode.window.activeTextEditor?.document.uri;
        if (active?.scheme === PDCLOC_SCHEME) {
            const real = realUriOf(active);
            statusItem.tooltip = new vscode.MarkdownString(
                `Decoded EU4 view over \`${real?.fsPath ?? 'unknown'}\` — click to open the raw file.`,
            );
            statusItem.show();
        } else {
            statusItem.hide();
        }
    };

    const openingDecodedViews = new Set<string>();
    const disposables: vscode.Disposable[] = [provider, statusItem, diagnostics];
    // Manual one-shot transcoding works with the transparent pipeline off;
    // with it on, the handlers answer with a pointer to the setting instead.
    disposables.push(
        vscode.commands.registerCommand('paradoxcode.localisation.encodeFile', (uri) =>
            void encodeFileManually(transcoder, uri, log),
        ),
        vscode.commands.registerCommand('paradoxcode.localisation.decodeFile', (uri) =>
            void decodeFileManually(transcoder, uri, log),
        ),
    );
    if (transparentEncodingEnabled()) {
        const autoOpenDocument = (document: vscode.TextDocument | undefined): void => {
            if (!document) {
                return;
            }
            void maybeAutoOpenDecodedView(document, openingDecodedViews).catch((error) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`transparentLoc: automatic decoded view failed: ${message}`);
            });
        };
        const updateEntryContext = (document: vscode.TextDocument | undefined): void => {
            if (!document) {
                return;
            }
            void updateDecodedEntryContext(document).catch((error) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`transparentLoc: decoded-view context update failed: ${message}`);
            });
        };
        disposables.push(
            vscode.workspace.registerFileSystemProvider(PDCLOC_SCHEME, provider, {
                // URIs under this scheme are byte-identical twins of their
                // `file://` counterparts (only the scheme differs), so the
                // provider must never fold case itself; case-insensitive
                // game-rule matching happens in the profile selectors above.
                isCaseSensitive: true,
            }),
            vscode.commands.registerCommand('paradoxcode.localisation.openDecoded', (uri) =>
                void openDecodedView(uri),
            ),
            vscode.commands.registerCommand('paradoxcode.localisation.revealOriginal', () =>
                void revealOriginal(),
            ),
            vscode.commands.registerCommand('paradoxcode.localisation.peekOriginal', () =>
                void peekOriginal(),
            ),
            vscode.commands.registerCommand('paradoxcode.localisation.endPeek', () =>
                void endPeek(),
            ),
            vscode.window.onDidChangeActiveTextEditor((editor) => {
                updateStatus();
                updateEntryContext(editor?.document);
                if (peek) {
                    const peekStillActive =
                        editor !== undefined && editor.document.uri.toString() === peek.real.toString();
                    if (!peekStillActive && editor?.viewColumn !== peek.column) {
                        // Focus moved to another group or out of the editors:
                        // flip back without stealing focus. A switch inside the
                        // peek's own group cannot rewrite a background tab, so
                        // the peek survives there until Esc.
                        void endPeek(false);
                    }
                }
                autoOpenDocument(editor?.document);
            }),
            vscode.window.tabGroups.onDidChangeTabs(() => {
                for (const key of manualRawViews) {
                    if (textTabsFor(vscode.Uri.parse(key)).length === 0) {
                        manualRawViews.delete(key);
                    }
                }
                if (peek && textTabsFor(peek.real).length === 0) {
                    // The peeked tab was closed outright — nothing to flip back to.
                    peek = undefined;
                    void vscode.commands.executeCommand('setContext', PEEKING_RAW_CONTEXT, false);
                }
            }),
            vscode.workspace.onDidOpenTextDocument(autoOpenDocument),
            vscode.workspace.onDidOpenTextDocument(updateEntryContext),
        );
        updateStatus();
        // `onLanguage` activation can happen after VS Code has already opened
        // the triggering document, so inspect the active editor as well as
        // relying on the document-open event.
        autoOpenDocument(vscode.window.activeTextEditor?.document);
        updateEntryContext(vscode.window.activeTextEditor?.document);
        log.appendLine('transparent localisation enabled (pdcloc:// provider active)');
    } else {
        log.appendLine(
            'transparent localisation disabled by configuration (manual encode/decode commands only)',
        );
    }

    const controller = vscode.Disposable.from(...disposables);
    context.subscriptions.push(controller);
    return controller;
}
