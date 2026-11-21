import * as fs from 'node:fs/promises';
import * as vscode from 'vscode';
import { ConversionRefused, convertBytes } from './byteCodec';
import { convertSelection } from './scriptSelection';
import { replaceFile } from './replaceFile';

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

/** Whole-file management applies only to EU4 localisation YML. */
function isLocalisationFile(path: string): boolean {
    return /(?:^|\/)localisation\/.*\.yml$/i.test(path.replace(/\\/g, '/'));
}

const utf8Bom = Buffer.from([0xef, 0xbb, 0xbf]);

async function encodeLocalisation(input: Uint8Array): Promise<Buffer> {
    const encoded = await convertBytes('localisation', 'encode', input);
    return encoded.subarray(0, 3).equals(utf8Bom) ? encoded : Buffer.concat([utf8Bom, encoded]);
}

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

/**
 * Whether `uri` is currently rendered inside a diff editor (git changes,
 * "Compare with…", `vscode.diff`). The decoded twin must never yank a tab out
 * from under a diff: `takeoverShow` cannot replace a diff tab (it only tracks
 * plain text tabs), so redirecting there pops duplicate tabs and re-fires on
 * every focus change. Diff viewers get the raw `file://` form; the
 * auto-redirect resumes once the diff closes.
 */
function isShownInDiff(uri: vscode.Uri): boolean {
    const target = uri.toString();
    for (const group of vscode.window.tabGroups.all) {
        for (const tab of group.tabs) {
            if (
                tab.input instanceof vscode.TabInputTextDiff
                && (tab.input.original.toString() === target || tab.input.modified.toString() === target)
            ) {
                return true;
            }
        }
    }
    return false;
}

class PdclocFileSystemProvider implements vscode.FileSystemProvider {
    private readonly emitter = new vscode.EventEmitter<vscode.FileChangeEvent[]>();
    private readonly watchers = new Set<vscode.FileSystemWatcher>();

    constructor(
        private readonly diagnostics: vscode.DiagnosticCollection,
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
        if (!real || !isLocalisationFile(real.fsPath)) {
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
        this.requireLocalisation(real);
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
        this.requireLocalisation(real);
        const original = await fs.readFile(real.fsPath);
        try {
            const decoded = await convertBytes('localisation', 'decode', original);
            this.diagnostics.delete(uri);
            return decoded;
        } catch (error) {
            if (!(error instanceof ConversionRefused) || error.offset === undefined) throw error;
            const line = lineAt(original, error.offset);
            const diagnostic = new vscode.Diagnostic(new vscode.Range(line, 0, line, 0),
                error.message, vscode.DiagnosticSeverity.Error);
            diagnostic.source = 'paradoxcode';
            diagnostic.code = 'LocalisationMixedEncoding';
            this.diagnostics.set(uri, [diagnostic]);
            return original;
        }
    }

    async writeFile(uri: vscode.Uri, content: Uint8Array): Promise<void> {
        const real = this.requireReal(uri);
        this.requireLocalisation(real);
        const before = await fs.stat(real.fsPath);
        const encoded = await encodeLocalisation(content);
        await replaceFile(real.fsPath, encoded, async () => {
            const current = await fs.stat(real.fsPath);
            if (current.mtimeMs !== before.mtimeMs || current.ctimeMs !== before.ctimeMs || current.size !== before.size) {
                throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('The file changed during conversion. Retry the save.'));
            }
        });
        this.diagnostics.delete(uri);
    }

    async readDirectory(): Promise<[string, vscode.FileType][]> {
        throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('pdcloc:// exposes individual files only'));
    }

    async createDirectory(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('pdcloc:// exposes individual files only'));
    }

    async delete(uri: vscode.Uri): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(
            vscode.l10n.t(
                'delete is not supported for pdcloc:// views ({0})',
                realUriOf(uri)?.fsPath ?? uri.toString(),
            ),
        );
    }

    async rename(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('rename is not supported for pdcloc:// views'));
    }

    async copy(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('copy is not supported for pdcloc:// views'));
    }

    private requireReal(uri: vscode.Uri): vscode.Uri {
        const real = realUriOf(uri);
        if (!real) {
            throw vscode.FileSystemError.FileNotFound(
                vscode.l10n.t('{0}:// requires a real backing path', PDCLOC_SCHEME),
            );
        }
        return real;
    }

    private requireLocalisation(uri: vscode.Uri): void {
        if (!isLocalisationFile(uri.fsPath)) {
            throw vscode.FileSystemError.NoPermissions(vscode.l10n.t('Select a YML file under localisation.'));
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

/** Search follows the chosen YML mode and preserves existing dirty buffers. */
export async function openTextDocumentForNavigation(uri: vscode.Uri): Promise<vscode.TextDocument> {
    if (uri.scheme === 'file' && isLocalisationFile(uri.fsPath) && automaticMode()
        && !manualRawViews.has(uri.toString()) && !hasDirtyDocument(uri)) {
        return vscode.workspace.openTextDocument(decodedUriOf(uri));
    }
    return vscode.workspace.openTextDocument(uri);
}

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

/**
 * Restores a full selection after a raw→decoded flip, keeping search-result
 * jumps and other programmatic opens on the row and column they targeted —
 * VS Code's built-in search indexes `file://` bytes only, so a jump always
 * lands on the raw file first and the flip must carry the cursor over. Lines
 * map one-to-one between the two forms (escape triples never contain newline
 * bytes); columns clamp where a collapsed triple shortened the line, since
 * raw and decoded columns diverge inside escaped spans.
 */
function restoreSelection(editor: vscode.TextEditor, selection: vscode.Selection): void {
    const clamp = (position: vscode.Position): vscode.Position => {
        const line = Math.min(Math.max(position.line, 0), editor.document.lineCount - 1);
        return new vscode.Position(
            line,
            Math.min(position.character, editor.document.lineAt(line).text.length),
        );
    };
    const restored = new vscode.Selection(clamp(selection.anchor), clamp(selection.active));
    editor.selection = restored;
    editor.revealRange(
        new vscode.Range(restored.active, restored.active),
        vscode.TextEditorRevealType.InCenterIfOutsideViewport,
    );
}

async function openDecodedView(uri: vscode.Uri | undefined): Promise<void> {
    const real = commandResource(uri);
    if (!automaticMode() || !real || real.scheme !== 'file' || !isLocalisationFile(real.fsPath)) {
        void vscode.window.showErrorMessage(
            vscode.l10n.t('Select a YML file under localisation.'),
        );
        return;
    }
    // Eligibility is the only gate for this explicit command: readFile
    // dispatches on the actual form — plain files pass through byte-identical,
    // escaped and mixed files decode, damaged files show as-is with an error.
    // Carry the cursor over: a search jump lands on the raw file first (with
    // its match selection), and the flip to the twin must not strand the user
    // at the top of the file instead of their target.
    const active = vscode.window.activeTextEditor;
    const selection =
        active?.document.uri.toString() === real.toString() ? active.selection : undefined;
    const target = await vscode.workspace.openTextDocument(decodedUriOf(real));
    const current = vscode.window.activeTextEditor;
    if (!automaticMode() || (active && current !== active)
        || vscode.workspace.textDocuments.some(doc => doc.uri.toString() === real.toString() && doc.isDirty)) return;
    const slot = textTabsFor(real)[0];
    const editor = await vscode.window.showTextDocument(target, { viewColumn: slot?.group.viewColumn, preview: slot?.tab.isPreview ?? false });
    if (!vscode.workspace.textDocuments.some(doc => doc.uri.toString() === real.toString() && doc.isDirty)) await closeTextTabs(real);
    supersedePeek(real);
    if (selection) {
        restoreSelection(editor, selection);
    }
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
            vscode.l10n.t(
                'ParadoxCode: raw view kept open — it has unsaved edits (Esc again after saving or discarding them).',
            ),
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

/** Honour legacy settings until the workspace chooses a management mode. */
function automaticMode(): boolean {
    const config = vscode.workspace.getConfiguration('paradoxcode.localisation');
    const mode = config.inspect<string>('encodingMode');
    const legacy = config.inspect<boolean>('transparentEncoding');
    for (const scope of ['workspaceFolderValue', 'workspaceValue', 'globalValue'] as const) {
        if (mode?.[scope] !== undefined) return mode[scope] === 'automatic';
        if (legacy?.[scope] !== undefined) return legacy[scope] === true && config.get('autoOpen') !== 'off';
    }
    return mode?.defaultValue === 'automatic' && config.get('autoOpen') !== 'off';
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

/** VS Code stores a UTF-8 signature outside getText(). Older hosts do not expose
 * TextDocument.encoding, so inspect just the on-disk signature on those hosts. */
async function manualBufferEncoding(document: vscode.TextDocument): Promise<string> {
    const encoding = (document as vscode.TextDocument & { encoding?: string }).encoding;
    if (encoding) return encoding;
    const configured = vscode.workspace.getConfiguration('files', document.uri).get<string>('encoding', 'utf8');
    if (configured !== 'utf8') return configured;
    const header = Buffer.alloc(utf8Bom.length);
    const file = await fs.open(document.uri.fsPath, 'r');
    try {
        await file.read(header, 0, header.length, 0);
        return header.equals(utf8Bom) ? 'utf8bom' : 'utf8';
    } finally {
        await file.close();
    }
}

/** Replace only the affected lines. A small edit in a mostly encoded file should not resend
 * the whole document to the editor, its undo stack and the language server. Line boundaries
 * keep CRLF and UTF-16 surrogate pairs intact. The codec has already validated the full input. */
function replaceConvertedLines(editor: vscode.TextEditor, before: string, after: string): Thenable<boolean> {
    let common = 0;
    const shorter = Math.min(before.length, after.length);
    while (common < shorter && before.charCodeAt(common) === after.charCodeAt(common)) common++;
    const start = common === 0 ? 0 : before.lastIndexOf('\n', common - 1) + 1;
    let end = before.length;
    let newEnd = after.length;
    while (end > common && newEnd > common && before.charCodeAt(end - 1) === after.charCodeAt(newEnd - 1)) {
        end--;
        newEnd--;
    }
    const newline = before.indexOf('\n', end);
    const lineEnd = newline < 0 ? before.length : newline + 1;
    const range = new vscode.Range(editor.document.positionAt(start), editor.document.positionAt(lineEnd));
    return editor.edit(edit => edit.replace(range, after.slice(start, newEnd + (lineEnd - end))));
}

async function convertFileManually(
    direction: 'encode' | 'decode',
    uri: vscode.Uri | undefined,
): Promise<void> {
    const real = commandResource(uri);
    const verb = direction === 'encode' ? vscode.l10n.t('encode') : vscode.l10n.t('decode');
    if (!real || real.scheme !== 'file' || !isLocalisationFile(real.fsPath)) {
        void vscode.window.showErrorMessage(vscode.l10n.t('Select a YML file under localisation.'));
        return;
    }
    if (automaticMode()) {
        void vscode.window.showInformationMessage(vscode.l10n.t(
            'ParadoxCode: transparent encoding is on — saves already encode automatically. '
            + 'Choose Manual encoding management to transcode by hand.',
        ));
        return;
    }
    const twin = decodedUriOf(real).toString();
    const dirtyTwin = () => vscode.workspace.textDocuments.some(doc => doc.isDirty && doc.uri.toString() === twin);
    try {
        if (dirtyTwin()) throw new Error(vscode.l10n.t('Finish editing the decoded view before converting the original buffer.'));
        const document = await vscode.workspace.openTextDocument(real);
        const editor = vscode.window.activeTextEditor?.document === document
            ? vscode.window.activeTextEditor : await vscode.window.showTextDocument(document);
        const version = document.version;
        const source = document.getText();
        const encoding = await manualBufferEncoding(document);
        if (encoding !== 'utf8' && encoding !== 'utf8bom') {
            throw new Error(vscode.l10n.t('Use UTF-8 or UTF-8 with BOM for this localisation buffer.'));
        }
        if (direction === 'decode' && !/[\x10-\x13]/.test(source)) return;
        const input = Buffer.from(source, 'utf8');
        // A plain UTF-8 editor needs the signature in its buffer. A BOM editor supplies
        // its own when saved. Decoding preserves whichever signature the editor uses.
        const output = direction === 'encode' && encoding === 'utf8'
            ? await encodeLocalisation(input)
            : await convertBytes('localisation', direction, input);
        if (document.version !== version || document.isClosed || automaticMode() || dirtyTwin()) {
            throw new Error(vscode.l10n.t('The document or encoding mode changed during conversion. Retry the command.'));
        }
        if (input.equals(output)) return;
        if (!await replaceConvertedLines(editor, source, output.toString('utf8'))) {
            throw new Error(vscode.l10n.t('The document or encoding mode changed during conversion. Retry the command.'));
        }
    } catch (error) {
        void vscode.window.showErrorMessage(vscode.l10n.t(
            'ParadoxCode: could not {0} this file: {1}.', verb, error instanceof Error ? error.message : String(error),
        ));
    }
}

const TRANSCODE_ELIGIBLE_CONTEXT = 'paradoxcode.transcodeEligible';

/** Path-only eligibility for whole-file YML commands. */
async function updateTranscodeEntryContext(
    document: vscode.TextDocument | undefined,
): Promise<void> {
    const eligible =
        document !== undefined &&
        document.uri.scheme === 'file' &&
        isLocalisationFile(document.uri.fsPath);
    await vscode.commands.executeCommand('setContext', TRANSCODE_ELIGIBLE_CONTEXT, eligible);
}

async function maybeAutoOpenDecodedView(
    document: vscode.TextDocument,
    opening: Set<string>,
): Promise<void> {
    if (document.uri.scheme !== 'file' || document.isDirty) {
        return;
    }
    const key = document.uri.toString();
    if (opening.has(key) || manualRawViews.has(key)) {
        return;
    }
    if (!isLocalisationFile(document.uri.fsPath)) {
        return;
    }
    // Only take over the editor the user is actually looking at: invisible
    // programmatic opens and quick tab switches must not pop a decoded tab.
    if (vscode.window.activeTextEditor?.document.uri.toString() !== key) {
        return;
    }
    // A document rendered inside a diff keeps its raw `file://` form — see
    // `isShownInDiff`. Focusing it after the diff closes re-runs this gate.
    if (isShownInDiff(document.uri)) {
        return;
    }
    opening.add(key);
    try {
        await openDecodedView(document.uri);
    } finally {
        opening.delete(key);
    }
}

/** Register manual and selection commands, and lazily enable the managed YML view. */
export async function activateTransparentLocalisation(
    context: vscode.ExtensionContext,
    log: vscode.OutputChannel,
): Promise<vscode.Disposable> {
    const diagnostics = vscode.languages.createDiagnosticCollection('paradoxcode.transparent');
    const provider = new PdclocFileSystemProvider(diagnostics);

    const statusItem = vscode.window.createStatusBarItem(
        'paradoxcode.transparentLoc',
        vscode.StatusBarAlignment.Left,
        99,
    );
    statusItem.name = 'ParadoxCode Decoded View';
    statusItem.text = `$(eye) ${vscode.l10n.t('EU4dll')}`;
    statusItem.command = 'paradoxcode.localisation.revealOriginal';

    const updateStatus = (): void => {
        const active = vscode.window.activeTextEditor?.document.uri;
        if (active?.scheme === PDCLOC_SCHEME) {
            const real = realUriOf(active);
            statusItem.tooltip = new vscode.MarkdownString(
                vscode.l10n.t(
                    'Save format: EU4dll. Click to view the raw file: {0}',
                    real?.fsPath ?? 'unknown',
                ),
            );
            statusItem.show();
        } else {
            statusItem.hide();
        }
    };

    const openingDecodedViews = new Set<string>();
    const disposables: vscode.Disposable[] = [provider, statusItem, diagnostics];
    // Manual button eligibility is independent of the experimental pipeline.
    // Always derive it from the active editor, including when no editor remains.
    const updateEntryContext = (): void => {
        void updateTranscodeEntryContext(vscode.window.activeTextEditor?.document).catch((error) => {
            const message = error instanceof Error ? error.message : String(error);
            log.appendLine(`transparentLoc: transcode button context update failed: ${message}`);
        });
    };
    // Manual one-shot transcoding works with the transparent pipeline off;
    // with it on, the handlers answer with a pointer to the setting instead.
    disposables.push(
        vscode.commands.registerCommand('paradoxcode.localisation.encodeFile', (uri) =>
            convertFileManually('encode', uri),
        ),
        vscode.commands.registerCommand('paradoxcode.localisation.decodeFile', (uri) =>
            convertFileManually('decode', uri),
        ),
        vscode.window.onDidChangeActiveTextEditor(updateEntryContext),
        vscode.workspace.onDidChangeWorkspaceFolders(updateEntryContext),
        vscode.commands.registerCommand('paradoxcode.localisation.encodeSelection', () => convertSelection('encode')),
        vscode.commands.registerCommand('paradoxcode.localisation.decodeSelection', () => convertSelection('decode')),
    );
    updateEntryContext();
    let registered = false;
    const updateMode = (): void => {
        const enabled = automaticMode();
        void vscode.commands.executeCommand('setContext', 'paradoxcode.automaticLocalisation', enabled);
        if (!enabled) {
            const active = vscode.window.activeTextEditor?.document;
            if (active?.uri.scheme === PDCLOC_SCHEME && !active.isDirty) {
                void revealOriginal().catch(error => log.appendLine(String(error)));
            }
            return;
        }
        manualRawViews.clear();
        if (registered) {
            const active = vscode.window.activeTextEditor?.document;
            if (active) void maybeAutoOpenDecodedView(active, openingDecodedViews).catch(error => log.appendLine(String(error)));
            return;
        }
        registered = true;
        const autoOpenDocument = (document: vscode.TextDocument | undefined): void => {
            if (!document || !automaticMode()) {
                return;
            }
            void maybeAutoOpenDecodedView(document, openingDecodedViews).catch((error) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`transparentLoc: automatic decoded view failed: ${message}`);
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
        );
        updateStatus();
        // `onLanguage` activation can happen after VS Code has already opened
        // the triggering document, so inspect the active editor as well as
        // relying on the document-open event.
        autoOpenDocument(vscode.window.activeTextEditor?.document);
    };
    disposables.push(vscode.workspace.onDidChangeConfiguration(change => {
        if (change.affectsConfiguration('paradoxcode.localisation.encodingMode')
            || change.affectsConfiguration('paradoxcode.localisation.transparentEncoding')) updateMode();
    }));
    updateMode();

    const controller = vscode.Disposable.from(...disposables);
    context.subscriptions.push(controller);
    return controller;
}
