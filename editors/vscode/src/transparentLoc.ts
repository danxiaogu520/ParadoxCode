// Transparent localisation read/write for EU4dll-transcoded files.
//
// The `pdcloc://` scheme mirrors a `file://` URI over the same real path:
// readFile decodes EU4dll escape triples to readable CJK text, writeFile
// re-encodes before the bytes touch disk, and the language server receives
// the decoded text through normal document sync (it resolves `pdcloc://`
// URIs to the backing path, so the virtual document hides the on-disk shard
// and every position is a decoded-view position — no offset mapping).
//
// Iron rule ② (never encode twice / decode twice) is enforced in both
// directions: decoding only happens for whole files the classifier marks
// `escaped`, and a save is refused outright when the buffer itself already
// contains escape sequences — with the reason surfaced to the user.
//
// Opening an eligible escaped file takes over its tab instead of adding a
// second one: the decoded view is shown in the raw tab's own slot (preview
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
    type Classification,
    type LocalisationProfile,
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

/** What readFile observed for a URI — consumed by the diagnostics publisher. */
interface ReadOutcome {
    classification: Classification;
    broken: number;
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
        const classification = this.transcoder.classify(bytes, profile);
        if (classification !== 'escaped') {
            // Iron rule ②: only whole files the classifier marks escaped get
            // decoded. Readable/Mixed content is shown as-is and reported.
            this.publishReadDiagnostics(uri, real, { classification, broken: 0 });
            return bytes;
        }
        const decoded = this.transcoder.decode(bytes, profile);
        if (decoded === 'invalid-utf8') {
            throw vscode.FileSystemError.Unavailable(
                'an escaped localisation file must be valid UTF-8 — the bytes are damaged',
            );
        }
        this.publishReadDiagnostics(uri, real, { classification, broken: decoded.broken });
        return decoded.text;
    }

    async writeFile(
        uri: vscode.Uri,
        content: Uint8Array,
        _options?: { create: boolean; overwrite: boolean },
    ): Promise<void> {
        const real = this.requireReal(uri);
        const profile = this.requireProfile(real);
        const classification = this.transcoder.classify(content, profile);
        if (classification === 'escaped' || classification === 'mixed') {
            // The buffer already contains escape sequences — this is pasted
            // transcoded text, and encoding it again would double-encode.
            const message =
                'Refused to save: the editor buffer already contains EU4dll escape sequences. ' +
                'Undo the paste (or decode the text first); transcoded files must be edited via their pdcloc:// view.';
            this.log.appendLine(
                `transparentLoc: refused save of ${real.fsPath} (${classification} buffer)`,
            );
            void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
            throw vscode.FileSystemError.NoPermissions(message);
        }
        const encoded = this.transcoder.encode(content, profile);
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
        outcome: ReadOutcome,
    ): void {
        const range = new vscode.Range(0, 0, 0, 0);
        let diagnostic: vscode.Diagnostic | undefined;
        if (outcome.classification === 'readable') {
            diagnostic = new vscode.Diagnostic(
                range,
                'This file sits on the game read path but is readable CJK text: the game will show ' +
                    'mojibake. Encode it (ParadoxCode: Transcode Localisation File) or keep it as the master copy.',
                vscode.DiagnosticSeverity.Warning,
            );
            diagnostic.code = 'LocalisationNotTranscoded';
        } else if (outcome.classification === 'mixed') {
            diagnostic = new vscode.Diagnostic(
                range,
                'This file mixes escaped triples with readable CJK (or has stray escape markers). ' +
                    'It is shown as-is without any transformation; fix the file manually.',
                vscode.DiagnosticSeverity.Error,
            );
            diagnostic.code = 'LocalisationMixedEncoding';
        } else if (outcome.classification === 'escaped' && outcome.broken > 0) {
            diagnostic = new vscode.Diagnostic(
                range,
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

async function openDecodedView(transcoder: Transcoder, uri: vscode.Uri | undefined): Promise<void> {
    const real = commandResource(uri);
    if (!real || real.scheme !== 'file') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: open a localisation yml (or a configured script file) first.',
        );
        return;
    }
    const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        void vscode.window.showErrorMessage(
            'ParadoxCode: this file is not eligible for the decoded view ' +
                '(localisation/**/*.yml or paradoxcode.localisation.transparentScriptGlobs).',
        );
        return;
    }
    const bytes = new Uint8Array(await fs.readFile(real.fsPath));
    const classification = transcoder.classify(bytes, profile);
    if (classification === 'mixed') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: this file mixes readable CJK with escape sequences; fix it manually first.',
        );
        return;
    }
    if (classification !== 'escaped') {
        // A normal readable UTF-8 (BOM included) or ASCII file has nothing to
        // decode — its decoded view would be a byte-identical copy.
        void vscode.window.showInformationMessage(
            'ParadoxCode: this file is readable UTF-8 already; the decoded view only applies to transcoded (escape-encoded) files.',
        );
        return;
    }
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

/**
 * Explicitly encodes a readable (master-copy) file into the on-disk escaped
 * form — the one-shot converter for files that live on the game read path but
 * were authored or pasted as readable CJK. The original is backed up next to
 * the file first.
 */
async function transcodeFile(
    transcoder: Transcoder,
    uri: vscode.Uri | undefined,
    log: vscode.OutputChannel,
): Promise<void> {
    const real = commandResource(uri);
    if (!real || real.scheme !== 'file') {
        void vscode.window.showErrorMessage('ParadoxCode: select a file to transcode first.');
        return;
    }
    const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        void vscode.window.showErrorMessage(
            'ParadoxCode: this file is not in the transparent-encoding scope ' +
                '(localisation/**/*.yml or paradoxcode.localisation.transparentScriptGlobs).',
        );
        return;
    }
    const bytes = new Uint8Array(await fs.readFile(real.fsPath));
    const classification = transcoder.classify(bytes, profile);
    if (classification === 'escaped') {
        void vscode.window.showInformationMessage(
            'ParadoxCode: file is already transcoded.',
        );
        return;
    }
    if (classification !== 'readable' && classification !== 'ascii') {
        void vscode.window.showErrorMessage(
            'ParadoxCode: file mixes readable and escaped content; fix it manually before transcoding.',
        );
        return;
    }
    const encoded = transcoder.encode(bytes, profile);
    if ('unencodable' in encoded) {
        const points = encoded.unencodable
            .map((point) => formatCodePoint(point.codePoint))
            .slice(0, 8)
            .join(', ');
        void vscode.window.showErrorMessage(
            `ParadoxCode: refused to transcode — ${encoded.unencodable.length} unencodable code point(s) (${points}).`,
        );
        return;
    }
    const backup = `${real.fsPath}.pre-transcode.bak`;
    await fs.copyFile(real.fsPath, backup);
    await fs.writeFile(real.fsPath, encoded.bytes);
    classificationStamps.delete(real.fsPath);
    // The file flipped readable → escaped, so the eye icon must appear now.
    void updateDecodedEntryContext(transcoder, vscode.window.activeTextEditor?.document);
    log.appendLine(`transparentLoc: transcoded ${real.fsPath} (backup: ${backup})`);
    void vscode.window.showInformationMessage(
        `ParadoxCode: transcoded ${nodePath.basename(real.fsPath)} (backup: ${nodePath.basename(backup)}).`,
    );
}

/** Whether opening an eligible raw file should automatically use its decoded twin. */
function autoOpenDecoded(): boolean {
    return vscode.workspace
        .getConfiguration('paradoxcode.localisation')
        .get<boolean>('autoOpenDecoded', true);
}

const TRANSCODE_ESCAPED_CONTEXT = 'paradoxcode.transcodeEscaped';

/** Cached escaped-classification behind the eye-icon context key. */
interface ClassificationStamp {
    mtime: number;
    size: number;
    profile: LocalisationProfile;
    escaped: boolean;
}

const classificationStamps = new Map<string, ClassificationStamp>();
let contextUpdateSequence = 0;

/**
 * Whether the file behind `real` is currently classifier-escaped, served from
 * (and refreshing) the mtime/size stamp cache — one disk read per file
 * version across every consumer (eye-icon context, auto-redirect).
 */
async function escapedOnDisk(transcoder: Transcoder, real: vscode.Uri): Promise<boolean> {
    const profile = profileForRealPath(real.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        return false;
    }
    try {
        const stats = await fs.stat(real.fsPath);
        const stamp = classificationStamps.get(real.fsPath);
        if (stamp && stamp.mtime === stats.mtimeMs && stamp.size === stats.size && stamp.profile === profile) {
            return stamp.escaped;
        }
        const bytes = new Uint8Array(await fs.readFile(real.fsPath));
        const escaped = transcoder.classify(bytes, profile) === 'escaped';
        classificationStamps.set(real.fsPath, { mtime: stats.mtimeMs, size: stats.size, profile, escaped });
        return escaped;
    } catch {
        return false;
    }
}

/**
 * Publishes `paradoxcode.transcodeEscaped` for a document: true only when the
 * file is both eligible and classifier-escaped. Normal readable UTF-8 (BOM
 * included) and ASCII files never offer the decoded view, yml and txt alike —
 * the same gate `openDecodedView` enforces, precomputed so menu `when`
 * clauses hide the eye icon instead of failing on click.
 */
async function updateDecodedEntryContext(
    transcoder: Transcoder,
    document: vscode.TextDocument | undefined,
): Promise<void> {
    const sequence = ++contextUpdateSequence;
    let escaped = false;
    if (document && document.uri.scheme === 'file') {
        escaped = await escapedOnDisk(transcoder, document.uri);
    }
    if (sequence !== contextUpdateSequence) {
        return;
    }
    await vscode.commands.executeCommand('setContext', TRANSCODE_ESCAPED_CONTEXT, escaped);
}

/**
 * Opens the decoded view when an eligible, transcoded file is opened through
 * its raw path, taking over the raw tab instead of adding a second one.
 * Readable, mixed, and ASCII files stay on the normal `file://`
 * URI so the classifier remains the single guard against double decoding.
 *
 * The `opening` set is only an in-flight guard. URIs deliberately pinned on
 * their raw view (revealOriginal, an active peek) are skipped until their tab
 * closes; a raw document can still be intentionally revealed later, and
 * closing/reopening it should apply the setting again.
 */
async function maybeAutoOpenDecodedView(
    transcoder: Transcoder,
    document: vscode.TextDocument,
    opening: Set<string>,
): Promise<void> {
    if (!autoOpenDecoded() || document.uri.scheme !== 'file') {
        return;
    }
    const key = document.uri.toString();
    if (opening.has(key) || manualRawViews.has(key)) {
        return;
    }
    opening.add(key);
    try {
        if (!(await escapedOnDisk(transcoder, document.uri))) {
            return;
        }
        // Only take over the editor the user is actually looking at: invisible
        // programmatic opens and quick tab switches must not pop a decoded tab.
        if (vscode.window.activeTextEditor?.document.uri.toString() !== key) {
            return;
        }
        await openDecodedView(transcoder, document.uri);
    } finally {
        opening.delete(key);
    }
}

/**
 * Registers the whole transparent-localisation feature: the FileSystemProvider,
 * commands, status-bar indicator, and client-side classification diagnostics.
 * Controlled by `paradoxcode.localisation.transparentEncoding`; automatic
 * raw-to-decoded redirection is controlled separately by
 * `paradoxcode.localisation.autoOpenDecoded`. When the artifact is missing
 * the feature degrades to a log line, never an error.
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
    const configuration = vscode.workspace.getConfiguration('paradoxcode.localisation');
    if (configuration.get<boolean>('transparentEncoding', true)) {
        const autoOpenDocument = (document: vscode.TextDocument | undefined): void => {
            if (!document) {
                return;
            }
            void maybeAutoOpenDecodedView(transcoder, document, openingDecodedViews).catch((error) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`transparentLoc: automatic decoded view failed: ${message}`);
            });
        };
        const updateEntryContext = (document: vscode.TextDocument | undefined): void => {
            if (!document) {
                return;
            }
            void updateDecodedEntryContext(transcoder, document).catch((error) => {
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
                void openDecodedView(transcoder, uri),
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
            vscode.commands.registerCommand('paradoxcode.localisation.transcodeFile', (uri) =>
                void transcodeFile(transcoder, uri, log),
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
            vscode.workspace.onDidSaveTextDocument(updateEntryContext),
        );
        updateStatus();
        // `onLanguage` activation can happen after VS Code has already opened
        // the triggering document, so inspect the active editor as well as
        // relying on the document-open event.
        autoOpenDocument(vscode.window.activeTextEditor?.document);
        updateEntryContext(vscode.window.activeTextEditor?.document);
        log.appendLine('transparent localisation enabled (pdcloc:// provider active)');
    } else {
        log.appendLine('transparent localisation disabled by configuration');
    }

    const controller = vscode.Disposable.from(...disposables);
    context.subscriptions.push(controller);
    return controller;
}
