// Transparent localisation read/write for EU4dll-transcoded files
// (`docs/eu4-cjk-localisation-design.md` §4).
//
// The `pdxloc://` scheme mirrors a `file://` URI over the same real path:
// readFile decodes EU4dll escape triples to readable CJK text, writeFile
// re-encodes before the bytes touch disk, and the language server receives
// the decoded text through normal document sync (it resolves `pdxloc://`
// URIs to the backing path, so the virtual document hides the on-disk shard
// and every position is a decoded-view position — no offset mapping).
//
// Iron rule ② (never encode twice / decode twice) is enforced in both
// directions: decoding only happens for whole files the classifier marks
// `escaped`, and a save is refused outright when the buffer itself already
// contains escape sequences — with the reason surfaced to the user.

import * as fs from 'node:fs/promises';
import * as nodePath from 'node:path';
import * as vscode from 'vscode';
import {
    PdxCodec,
    PROFILE_LOCALISATION,
    PROFILE_SCRIPT,
    type Classification,
    type LocalisationProfile,
} from './pdxCodec';

export const PDXLOC_SCHEME = 'pdxloc';

/** The `pdxloc://` twin of a real `file://` URI. */
export function decodedUriOf(real: vscode.Uri): vscode.Uri {
    return real.with({ scheme: PDXLOC_SCHEME });
}

/** The real `file://` URI behind a `pdxloc://` view, if any. */
export function realUriOf(uri: vscode.Uri): vscode.Uri | undefined {
    if (uri.scheme !== PDXLOC_SCHEME) {
        return undefined;
    }
    return uri.with({ scheme: 'file' });
}

function globToRegExp(pattern: string): RegExp {
    const normalized = pattern.replace(/\\/g, '/');
    const escaped = normalized
        .replace(/[.+^${}()|[\]\\]/g, '\\$&')
        .replace(/\*\*/g, '\u0000')
        .replace(/\*/g, '[^/]*')
        .replace(/\u0000/g, '.*')
        .replace(/\?/g, '.');
    return new RegExp(`^${escaped}$`, 'i');
}

/**
 * Selects the codec profile for a real file: localisation yml under a
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
        .get<string[]>('transparentScriptGlobs', ['history/**'])
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

class PdxlocFileSystemProvider implements vscode.FileSystemProvider {
    private readonly emitter = new vscode.EventEmitter<vscode.FileChangeEvent[]>();
    private readonly watchers = new Set<vscode.FileSystemWatcher>();

    constructor(
        private readonly codec: PdxCodec,
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
        const classification = this.codec.classify(bytes, profile);
        if (classification !== 'escaped') {
            // Iron rule ②: only whole files the classifier marks escaped get
            // decoded. Readable/Mixed content is shown as-is and reported.
            this.publishReadDiagnostics(uri, real, { classification, broken: 0 });
            return bytes;
        }
        const decoded = this.codec.decode(bytes, profile);
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
        const classification = this.codec.classify(content, profile);
        if (classification === 'escaped' || classification === 'mixed') {
            // The buffer already contains escape sequences — this is pasted
            // transcoded text, and encoding it again would double-encode.
            const message =
                'Refused to save: the editor buffer already contains EU4dll escape sequences. ' +
                'Undo the paste (or decode the text first); transcoded files must be edited via their pdxloc:// view.';
            this.log.appendLine(
                `transparentLoc: refused save of ${real.fsPath} (${classification} buffer)`,
            );
            void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
            throw vscode.FileSystemError.NoPermissions(message);
        }
        const encoded = this.codec.encode(content, profile);
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
        throw vscode.FileSystemError.NoPermissions('pdxloc:// exposes individual files only');
    }

    async createDirectory(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('pdxloc:// exposes individual files only');
    }

    async delete(uri: vscode.Uri): Promise<void> {
        throw vscode.FileSystemError.NoPermissions(
            `delete is not supported for pdxloc:// views (${realUriOf(uri)?.fsPath ?? uri.toString()})`,
        );
    }

    async rename(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('rename is not supported for pdxloc:// views');
    }

    async copy(): Promise<void> {
        throw vscode.FileSystemError.NoPermissions('copy is not supported for pdxloc:// views');
    }

    private requireReal(uri: vscode.Uri): vscode.Uri {
        const real = realUriOf(uri);
        if (!real) {
            throw vscode.FileSystemError.FileNotFound(
                `${PDXLOC_SCHEME}:// requires a real backing path`,
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
    const document = await vscode.workspace.openTextDocument(decodedUriOf(real));
    await vscode.window.showTextDocument(document);
}

async function revealOriginal(): Promise<void> {
    const active = vscode.window.activeTextEditor?.document.uri;
    const real = active ? realUriOf(active) : undefined;
    if (!real) {
        return;
    }
    await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(real));
}

/**
 * Explicitly encodes a readable (master-copy) file into the on-disk escaped
 * form — the one-shot converter for files that live on the game read path but
 * were authored or pasted as readable CJK. The original is backed up next to
 * the file first.
 */
async function transcodeFile(
    codec: PdxCodec,
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
    const classification = codec.classify(bytes, profile);
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
    const encoded = codec.encode(bytes, profile);
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
    log.appendLine(`transparentLoc: transcoded ${real.fsPath} (backup: ${backup})`);
    void vscode.window.showInformationMessage(
        `ParadoxCode: transcoded ${nodePath.basename(real.fsPath)} (backup: ${nodePath.basename(backup)}).`,
    );
}

/**
 * Offers the decoded view when a transcoded file is opened through its raw
 * path — the most common entry point for `localisation/replace/*.yml`.
 * Each file is only asked once per session.
 */
async function maybePromptDecodedView(
    codec: PdxCodec,
    document: vscode.TextDocument,
    prompted: Set<string>,
): Promise<void> {
    const key = document.uri.toString();
    if (prompted.has(key) || document.uri.scheme !== 'file') {
        return;
    }
    const profile = profileForRealPath(document.uri.fsPath, transparentScriptGlobs());
    if (profile === undefined) {
        return;
    }
    let bytes: Uint8Array;
    try {
        bytes = new Uint8Array(await fs.readFile(document.uri.fsPath));
    } catch {
        return;
    }
    if (codec.classify(bytes, profile) !== 'escaped') {
        return;
    }
    prompted.add(key);
    const choice = await vscode.window.showInformationMessage(
        'ParadoxCode: this file contains EU4dll escape sequences. Open it as readable Chinese?',
        'Open decoded view',
    );
    if (choice === 'Open decoded view') {
        await openDecodedView(document.uri);
    }
}

/**
 * Registers the whole transparent-localisation feature: the FileSystemProvider,
 * commands, status-bar indicator, and client-side classification diagnostics.
 * Controlled by `paradoxcode.localisation.transparentEncoding`; when the
 * artifact is missing the feature degrades to a log line, never an error.
 */
export async function activateTransparentLocalisation(
    context: vscode.ExtensionContext,
    log: vscode.OutputChannel,
): Promise<vscode.Disposable> {
    const codec = new PdxCodec();

    const diagnostics = vscode.languages.createDiagnosticCollection('paradoxcode.transparent');
    const provider = new PdxlocFileSystemProvider(codec, diagnostics, log);

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
        if (active?.scheme === PDXLOC_SCHEME) {
            const real = realUriOf(active);
            statusItem.tooltip = new vscode.MarkdownString(
                `Decoded EU4 view over \`${real?.fsPath ?? 'unknown'}\` — click to open the raw file.`,
            );
            statusItem.show();
        } else {
            statusItem.hide();
        }
    };

    const promptedDecodedViews = new Set<string>();
    const disposables: vscode.Disposable[] = [provider, statusItem, diagnostics];
    const configuration = vscode.workspace.getConfiguration('paradoxcode.localisation');
    if (configuration.get<boolean>('transparentEncoding', true)) {
        disposables.push(
            vscode.workspace.registerFileSystemProvider(PDXLOC_SCHEME, provider, {
                isCaseSensitive: true,
            }),
            vscode.commands.registerCommand('paradoxcode.localisation.openDecoded', (uri) =>
                void openDecodedView(uri),
            ),
            vscode.commands.registerCommand('paradoxcode.localisation.revealOriginal', () =>
                void revealOriginal(),
            ),
            vscode.commands.registerCommand('paradoxcode.localisation.transcodeFile', (uri) =>
                void transcodeFile(codec, uri, log),
            ),
            vscode.window.onDidChangeActiveTextEditor(updateStatus),
            vscode.workspace.onDidOpenTextDocument((document) =>
                void maybePromptDecodedView(codec, document, promptedDecodedViews),
            ),
        );
        updateStatus();
        log.appendLine('transparent localisation enabled (pdxloc:// provider active)');
    } else {
        log.appendLine('transparent localisation disabled by configuration');
    }

    const controller = vscode.Disposable.from(...disposables);
    context.subscriptions.push(controller);
    return controller;
}
