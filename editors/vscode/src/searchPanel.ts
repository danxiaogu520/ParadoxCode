import * as fs from 'fs';
import * as path from 'path';
import { randomBytes } from 'crypto';
import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/node';
import { openTextDocumentForNavigation, realUriOf } from './transparentLoc';
import { searchPanelStrings, webviewI18nMessage } from './webviewI18n';
import type { WorkspaceFileRoot } from './fileExplorer';

interface SearchLocation {
    uri: string;
    path: string;
    expectedText?: string;
    expectedName?: string;
    lineAnchor?: number;
    sourceLines?: number;
    range: { start: { line: number; character: number }; end: { line: number; character: number } };
}
interface SearchEntry {
    id: string;
    name: string;
    kind: string;
    location?: SearchLocation;
    version?: number;
    active?: boolean;
    dirty?: boolean;
    revision?: number;
    epoch?: number;
    corpusGeneration?: number;
}
interface SearchPage {
    revision: number;
    snapshot: string;
    groups: { id: string; versions: SearchEntry[] }[];
    nextOffset: number | null;
    limitations: string[];
}

/** One retained panel; only server-issued result identities can open documents or copy names. */
export class SearchPanel implements vscode.Disposable {
    private panel: vscode.WebviewPanel | undefined;
    private cancellation: vscode.CancellationTokenSource | undefined;
    private readonly detailTokens = new Set<vscode.CancellationTokenSource>();
    private readonly entryClients = new Map<string, LanguageClient>();
    private generation = 0;
    private entries = new Map<string, SearchEntry>();
    private readonly tabEntries = new Map<string, Set<string>>();
    private referenceSnapshot: string | undefined;
    private referenceTarget: { id: string; roots: unknown } | undefined;
    private contextSequence = 0;
    private contextCancellation: vscode.CancellationTokenSource | undefined;
    private readonly subscriptions: vscode.Disposable[];
    private corpusEpoch = Date.now();
    private sourceWatchers: vscode.Disposable[] = [];
    private watchedRoots = '';
    private roots: WorkspaceFileRoot[] = [];
    private refreshTimer: NodeJS.Timeout | undefined;

    public constructor(
        private readonly extensionUri: vscode.Uri,
        private readonly getClient: () => LanguageClient | undefined,
        private readonly isReady: () => boolean = () => true,
    ) {
        const refresh = (diskChanged = false) => {
            if (!this.panel) return;
            this.cancel();
            if (diskChanged) this.corpusEpoch++;
            clearTimeout(this.refreshTimer);
            this.refreshTimer = setTimeout(() => {
                this.post({ type: 'invalidate' });
            }, 250);
        };
        this.subscriptions = [
            vscode.workspace.onDidChangeTextDocument(() => refresh()),
            vscode.workspace.onDidSaveTextDocument(() => refresh(true)),
            vscode.workspace.onDidCloseTextDocument(() => refresh()),
            vscode.workspace.onDidChangeConfiguration(event => {
                if (event.affectsConfiguration('paradoxcode')) refresh(true);
            }),
        ];
    }

    public show(): void {
        if (this.panel) {
            this.panel.reveal(vscode.ViewColumn.Beside);
            this.post({ type: 'focus' });
            return;
        }
        const editor = vscode.window.activeTextEditor;
        const selection = editor?.document.getText(editor.selection) ?? '';
        const panel = vscode.window.createWebviewPanel('paradoxcode.search', vscode.l10n.t('ParadoxCode Search'), vscode.ViewColumn.Beside, {
            enableScripts: true,
            retainContextWhenHidden: true,
            localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'media')],
        });
        this.panel = panel;
        panel.webview.onDidReceiveMessage((message: unknown) => {
            if (!message || typeof message !== 'object') return;
            const input = message as Record<string, unknown>;
            switch (input.type) {
                case 'ready':
                    this.post(webviewI18nMessage(searchPanelStrings()));
                    this.post({ type: 'initial', query: selection.slice(0, 1024) });
                    void this.context();
                    return;
                case 'context': void this.context(); return;
                case 'search': void this.search(input.params, input.sequence); return;
                case 'cancel': this.cancel(); return;
                case 'detail': if (typeof input.id === 'string') void this.detail(input.id); return;
                case 'open': if (typeof input.id === 'string') void this.open(input.id); return;
                case 'copy': {
                    const entry = typeof input.id === 'string' ? this.entries.get(input.id) : undefined;
                    if (entry) void vscode.env.clipboard.writeText(entry.name);
                    return;
                }
                case 'references': if (typeof input.id === 'string') { this.referenceTarget = { id: input.id, roots: input.roots }; void this.references(input.id, input.roots); } return;
                case 'moreReferences': if (this.referenceTarget && typeof input.offset === 'number') void this.references(this.referenceTarget.id, this.referenceTarget.roots, input.offset); return;
            }
        });
        panel.onDidDispose(() => {
            if (this.panel !== panel) return;
            this.cancel();
            clearTimeout(this.refreshTimer);
            this.entries.clear();
            this.entryClients.clear();
            this.tabEntries.clear();
            this.contextCancellation?.cancel();
            this.contextCancellation?.dispose();
            this.contextCancellation = undefined;
            for (const watcher of this.sourceWatchers) watcher.dispose();
            this.sourceWatchers = []; this.watchedRoots = '';
            this.panel = undefined;
        });
        const nonce = randomBytes(16).toString('hex');
        const uri = (file: string) => panel.webview.asWebviewUri(vscode.Uri.joinPath(this.extensionUri, 'media', file)).toString();
        panel.webview.html = fs.readFileSync(path.join(this.extensionUri.fsPath, 'media', 'search.html'), 'utf8')
            .replaceAll('{{cspSource}}', panel.webview.cspSource)
            .replaceAll('{{nonce}}', nonce)
            .replaceAll('{{styleUri}}', uri('search.css'))
            .replaceAll('{{scriptUri}}', uri('search.js'));
    }

    private post(message: unknown): void { void this.panel?.webview.postMessage(message); }

    private cancel(): void {
        this.generation++;
        this.cancellation?.cancel();
        this.cancellation?.dispose();
        this.cancellation = undefined;
        for (const token of this.detailTokens) { token.cancel(); token.dispose(); }
        this.detailTokens.clear();
    }

    private async context(): Promise<void> {
        const client = this.getClient();
        if (!client) {
            this.post({ type: 'error', message: vscode.l10n.t('The language server is not running. Reload the server and retry.') });
            return;
        }
        if (!this.isReady()) { this.post({ type: 'indexing' }); return; }
        const panel = this.panel;
        const contextSequence = ++this.contextSequence;
        this.contextCancellation?.cancel();
        this.contextCancellation?.dispose();
        const token = new vscode.CancellationTokenSource();
        this.contextCancellation = token;
        try {
            const context = await client.sendRequest('pdc/editorSearchContext', token.token);
            if (contextSequence === this.contextSequence && panel === this.panel && client === this.getClient()) {
                const roots = (context as { roots: WorkspaceFileRoot[] }).roots;
                this.nameRoots(roots);
                this.watchSources(roots);
                this.post({ type: 'context', context });
            }
        } catch (error) {
            if (!token.token.isCancellationRequested && contextSequence === this.contextSequence && panel === this.panel) this.post({ type: 'error', message: String(error) });
        } finally { if (this.contextCancellation === token) this.contextCancellation = undefined; token.dispose(); }
    }

    private nameRoots(roots: WorkspaceFileRoot[]): void {
        const base = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        const configured = vscode.workspace.getConfiguration?.('paradoxcode')
            .get<{ id: string; path: string }[]>('dependencies', []) ?? [];
        const canonical = (value: string) => {
            const absolute = path.isAbsolute(value) ? value : path.resolve(base ?? '.', value);
            const resolved = path.normalize(absolute);
            return process.platform === 'win32' ? resolved.toLowerCase() : resolved;
        };
        for (const root of roots) {
            if (root.kind !== 'dependency') continue;
            const names = configured.filter(item => typeof item.id === 'string' && typeof item.path === 'string'
                && (path.isAbsolute(item.path) || base !== undefined) && canonical(item.path) === canonical(root.path));
            if (names.length === 1) root.name = names[0].id;
        }
    }

    private async search(params: unknown, sequence: unknown): Promise<void> {
        if (!params || typeof params !== 'object' || typeof sequence !== 'number') return;
        this.cancel();
        const client = this.getClient();
        if (!client) {
            this.post({ type: 'error', sequence, message: vscode.l10n.t('The language server is not running. Reload the server and retry.') });
            return;
        }
        if (!this.isReady()) { this.post({ type: 'indexing', sequence }); return; }
        const generation = this.generation;
        const token = new vscode.CancellationTokenSource();
        this.cancellation = token;
        try {
            const request = params as { offset?: number; tab: string; roots?: number[] };
            const buffers = request.tab === 'text' ? this.foreignBuffers(request.roots) : { buffers: [], limitations: [] };
            const response = await client.sendRequest<SearchPage>('pdc/editorSearch', {
                ...params, buffers: buffers.buffers, corpusEpoch: this.corpusEpoch,
            }, token.token);
            if (generation !== this.generation || client !== this.getClient()) return;
            response.limitations.push(...buffers.limitations);
            let tabEntries = this.tabEntries.get(request.tab);
            if (!request.offset) {
                if (tabEntries) for (const id of tabEntries) { this.entries.delete(id); this.entryClients.delete(id); }
                tabEntries = new Set();
                this.tabEntries.set(request.tab, tabEntries);
            }
            for (const group of response.groups) for (const entry of group.versions) {
                entry.revision = response.revision;
                entry.epoch = this.corpusEpoch;
                entry.dirty = vscode.workspace.textDocuments.some(document => document.uri.toString() === entry.location?.uri && document.isDirty);
                this.entries.set(entry.id, entry); this.entryClients.set(entry.id, client); tabEntries?.add(entry.id);
            }
            if (request.tab !== 'rules') response.limitations.push(...this.conflicts());
            this.post({ type: 'results', sequence, response });
        } catch (error) {
            if (generation !== this.generation) return;
            const code = (error as { code?: number }).code;
            if (code === -32801) {
                this.entries.clear();
                this.post({ type: 'invalidate' });
            } else if (code === -32800) {
                this.post({ type: 'cancelled', sequence });
            } else {
                this.post({ type: 'error', sequence, message: String(error) });
            }
        } finally {
            if (this.cancellation === token) this.cancellation = undefined;
            token.dispose();
        }
    }

    private async open(id: string): Promise<void> {
        const entry = this.entries.get(id);
        if (!entry?.location) return;
        const client = this.getClient();
        if (!client) return;
        if (this.entryClients.get(id) !== client || entry.epoch !== this.corpusEpoch) { this.refresh(); return; }
        const generation = this.generation;
        const revision = entry.revision;
        try {
            const current = await client.sendRequest<{ revision: number }>('pdc/editorSearchContext');
            if (generation !== this.generation || client !== this.getClient() || current.revision !== revision) {
                this.post({ type: 'invalidate' });
                return;
            }
            const uri = vscode.Uri.parse(entry.location.uri);
            const document = await openTextDocumentForNavigation(uri);
            if (generation !== this.generation || client !== this.getClient() || (entry.version != null && document.version !== entry.version)) {
                this.post({ type: 'invalidate' });
                return;
            }
            const { start, end } = entry.location.range;
            let range = new vscode.Range(start.line, start.character, end.line, end.character);
            if (entry.location.expectedText != null || entry.location.expectedName != null) {
                const text = document.getText(range);
                if ((entry.location.expectedText != null && text !== entry.location.expectedText)
                    || (entry.location.expectedName != null && text.replace(/^"|"$/g, '') !== entry.location.expectedName)) {
                    const line = start.line < document.lineCount ? document.lineAt(start.line).text : undefined;
                    const expected = entry.location.expectedText ?? entry.location.expectedName ?? '';
                    const at = line?.indexOf(expected) ?? -1;
                    if (expected !== '' && !expected.includes('\n') && at >= 0 && line?.lastIndexOf(expected) === at) {
                        // BOM removal and a different character representation can change columns.
                        range = new vscode.Range(start.line, at, start.line, at + expected.length);
                    } else if (line !== undefined && entry.location.sourceLines === document.lineCount
                        && entry.location.lineAnchor != null && asciiLineAnchor(line) === entry.location.lineAnchor) {
                        range = new vscode.Range(start.line, 0, start.line, 0);
                        this.post({ type: 'navigationNotice' });
                    } else {
                        this.refresh();
                        this.post({ type: 'error', message: vscode.l10n.t('The result changed on disk. Refresh the index and search again.') });
                        return;
                    }
                }
            }
            const viewColumn = vscode.window.visibleTextEditors?.find(editor => editor.viewColumn !== this.panel?.viewColumn)?.viewColumn
                ?? (this.panel?.viewColumn === vscode.ViewColumn.One ? vscode.ViewColumn.Beside : vscode.ViewColumn.One);
            await vscode.window.showTextDocument(document, { viewColumn, preview: true, preserveFocus: true, selection: range });
        } catch (error) { this.post({ type: 'error', message: String(error) }); }
    }

    private async references(id: string, roots: unknown, offset = 0): Promise<void> {
        const entry = this.entries.get(id);
        const client = this.getClient();
        if (!entry || !client) return;
        if (this.entryClients.get(id) !== client || entry.epoch !== this.corpusEpoch) { this.refresh(); return; }
        this.cancel();
        const generation = this.generation;
        const token = new vscode.CancellationTokenSource();
        this.cancellation = token;
        try {
            const response = await client.sendRequest<SearchPage>('pdc/editorSearchReferences', {
                id: entry.id, name: entry.name, kind: entry.kind,
                revision: entry.revision, corpusEpoch: entry.epoch, roots, offset, snapshot: offset ? this.referenceSnapshot : undefined,
            }, token.token);
            if (generation !== this.generation || client !== this.getClient()) return;
            for (const group of response.groups) for (const result of group.versions) {
                result.revision = response.revision; result.epoch = this.corpusEpoch;
                this.entries.set(result.id, result); this.entryClients.set(result.id, client);
            }
            this.referenceSnapshot = response.snapshot;
            this.post({ type: 'references', name: entry.name, kind: entry.kind, offset, response });
        } catch (error) {
            if (generation === this.generation) { if ((error as { code?: number }).code === -32801) this.post({ type: 'invalidate' }); else if ((error as { code?: number }).code !== -32800) this.post({ type: 'error', message: String(error) }); }
        } finally { if (this.cancellation === token) this.cancellation = undefined; token.dispose(); }
    }

    /** Refresh source choices and invalidate ranges after an index or source change. */
    public refresh(): void {
        this.cancel(); this.corpusEpoch++;
        if (this.panel) this.post({ type: 'invalidate' });
    }

    private watchSources(roots: WorkspaceFileRoot[]): void {
        this.roots = roots;
        const identity = JSON.stringify(roots.map(root => root.path));
        if (identity === this.watchedRoots) return;
        for (const watcher of this.sourceWatchers) watcher.dispose();
        this.sourceWatchers = []; this.watchedRoots = identity;
        const changed = () => {
            this.cancel(); this.corpusEpoch++;
            clearTimeout(this.refreshTimer);
            this.refreshTimer = setTimeout(() => this.post({ type: 'invalidate' }), 250);
        };
        for (const root of roots) {
            const watcher = vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(root.path, '**/*'));
            this.sourceWatchers.push(watcher, watcher.onDidCreate(changed), watcher.onDidChange(changed), watcher.onDidDelete(changed));
        }
    }

    /** Documents outside the language-client selectors still own their unsaved text. */
    private foreignBuffers(selected: number[] | undefined): {
        buffers: { uri: string; version: number; text: string }[]; limitations: string[];
    } {
        const buffers: { uri: string; version: number; text: string }[] = [];
        const limitations: string[] = [];
        let bytes = 0;
        for (const document of vscode.workspace.textDocuments) {
            if (!document.isDirty || document.languageId === 'eu4' || document.languageId === 'localisation') continue;
            const real = realUriOf(document.uri) ?? document.uri;
            if (real.scheme !== 'file' || !this.roots.some(root => {
                if (selected && !selected.includes(root.id)) return false;
                const relative = path.relative(root.path, real.fsPath);
                return relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
            })) continue;
            const buffer = { uri: document.uri.toString(), version: document.version, text: document.getText() };
            const cost = Buffer.byteLength(JSON.stringify(buffer), 'utf8');
            if (buffer.uri.length > 8192 || Buffer.byteLength(buffer.text, 'utf8') > 2 * 1024 * 1024
                || bytes + cost > 8 * 1024 * 1024 || buffers.length >= 128) {
                limitations.push(vscode.l10n.t('An unsaved buffer exceeds the search budget: {0}', buffer.uri.slice(0, 200)));
                continue;
            }
            buffers.push(buffer); bytes += cost;
        }
        return { buffers, limitations };
    }

    private async detail(id: string): Promise<void> {
        const entry = this.entries.get(id);
        const client = this.getClient();
        if (!entry || !client) return;
        if (this.entryClients.get(id) !== client || entry.epoch !== this.corpusEpoch) { this.refresh(); return; }
        const generation = this.generation;
        const token = new vscode.CancellationTokenSource();
        this.detailTokens.add(token);
        try {
            const response = await client.sendRequest('pdc/editorSearchDetail', { id, revision: entry.revision, corpusEpoch: entry.epoch, corpusGeneration: entry.corpusGeneration }, token.token);
            if (generation === this.generation && client === this.getClient()) this.post({ type: 'detail', response });
        } catch (error) {
            if (!token.token.isCancellationRequested && generation === this.generation) { if ((error as {code?:number}).code === -32801) this.refresh(); else this.post({ type: 'error', message: String(error) }); }
        } finally { this.detailTokens.delete(token); token.dispose(); }
    }

    private conflicts(): string[] {
        const dirty = new Map<string, number>();
        for (const document of vscode.workspace.textDocuments) {
            if (!document.isDirty) continue;
            const key = (realUriOf(document.uri) ?? document.uri).toString();
            dirty.set(key, (dirty.get(key) ?? 0) + 1);
        }
        return [...dirty].filter(([, count]) => count > 1).map(([uri]) => vscode.l10n.t(
            'Both views of {0} have unsaved changes. Search uses the existing decoded-document owner; review both buffers.', uri,
        ));
    }

    public dispose(): void {
        this.cancel();
        clearTimeout(this.refreshTimer);
        this.contextCancellation?.cancel();
        this.contextCancellation?.dispose();
        for (const watcher of this.sourceWatchers) watcher.dispose();
        this.sourceWatchers = [];
        this.panel?.dispose();
        for (const subscription of this.subscriptions) subscription.dispose();
    }
}

/** A conservative line anchor ignores bytes whose display depends on the source encoding. */
function asciiLineAnchor(text: string): number {
    let hash = 0x811c9dc5;
    for (const character of text) {
        const code = character.codePointAt(0)!;
        if (code >= 0x20 && code <= 0x7e) hash = Math.imul(hash ^ code, 16777619) >>> 0;
    }
    return hash;
}
