import { randomUUID } from 'node:crypto';
import * as vscode from 'vscode';
import type { Location } from 'vscode-languageserver-protocol';
import type { LanguageClient } from 'vscode-languageclient/node';
import { pngDataUrl } from './gameAssets';
import { composeEventCard, isPreviewLocation, parseHoverCardResponse, parseLocFormat, type HoverCardWire } from './hoverCards';
import { MissionPreviewPanel } from './previewPanel';
import { openTextDocumentForNavigation } from './transparentLoc';

export interface PreviewTarget {
    kind: 'event' | 'mission';
    id: string;
    title?: string;
    location: Location;
    path?: string;
}

export function parsePreviewTarget(value: unknown): PreviewTarget | undefined {
    const t = value as Partial<PreviewTarget> | undefined;
    if (!t || (t.kind !== 'event' && t.kind !== 'mission') || !t.id || typeof t.id !== 'string'
        || !isPreviewLocation(t.location) || !t.location.uri.startsWith('file:')) return undefined;
    return { kind: t.kind, id: t.id, title: typeof t.title === 'string' ? t.title : undefined,
        location: { ...t.location, uri: vscode.Uri.parse(t.location.uri).toString() },
        path: typeof t.path === 'string' ? t.path : undefined };
}

function escapeHtml(text: string): string {
    return text.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

async function eventPage(card: HoverCardWire, generation: number) {
    const event = card.event!;
    const store = MissionPreviewPanel.store();
    const assets = card.cardAssets ?? {};
    const load = (a?: HoverCardWire['asset']) => a && !a.archiveMember ? store.textureRaster(a.path) : undefined;
    const [backgroundTop, picture] = await Promise.all([load(assets.backgroundTop), load(card.asset)]);
    const locations: Location[] = [];
    const button = (label: string, location?: Location, style = '') => {
        const formatted = parseLocFormat(label);
        const text = formatted.segments.map(s => s.color
            ? '<span style="color:' + s.color + '">' + escapeHtml(s.text) + '</span>' : escapeHtml(s.text)).join('');
        if (!location) return style ? '' : '<span>' + text + '</span>';
        const i = locations.push(location) - 1;
        return '<button data-index="' + i + '" ' + style + '>' + (style ? escapeHtml(formatted.plain) : text) + '</button>';
    };
    const source = (key?: string | null) => key && event.sources && Object.hasOwn(event.sources, key) ? event.sources[key] : undefined;
    const desc = event.desc?.value || event.descKey || '';
    const title = event.title?.value || event.titleKey || event.id;
    let body: string | undefined;
    // Arbitrarily long mod text stays readable as HTML without creating an unbounded bitmap.
    if ((backgroundTop || picture) && desc.length <= 8192 && event.options.length <= 64) {
        const fonts = await store.loadFontRasters();
        const canDraw = [title, desc, ...event.options.map(o => o.name?.value || o.nameKey)].every(label =>
            [...parseLocFormat(label).plain].every(char => /\s/.test(char) ||
                fonts.chinese?.chars.has(char.codePointAt(0)!) || fonts.english?.chars.has(char.codePointAt(0)!)));
        if (canDraw) {
            const [backgroundMiddle, backgroundBottomS, backgroundBottomM, backgroundBottomL, optionButton] = await Promise.all([
                load(assets.backgroundMiddle), load(assets.backgroundBottomS), load(assets.backgroundBottomM),
                load(assets.backgroundBottomL), load(assets.optionButton)]);
            const zones: { key: string; label: string; top: number; height: number }[] = [];
            const raster = composeEventCard(event, { backgroundTop, backgroundMiddle, backgroundBottomS, backgroundBottomM,
                backgroundBottomL, optionButton, picture, fonts }, (key, label, top, height) => zones.push({ key, label, top, height }));
            if (raster) body = '<div class="window"><img alt="' + escapeHtml(title) + '" src="' + pngDataUrl(raster) + '">' +
                zones.filter(z => z.height > 0).map(z => button(z.label, source(z.key), 'class="area" style="top:' +
                    (100 * z.top / raster.height) + '%;height:' + (100 * z.height / raster.height) + '%"')).join('') + '</div>';
        }
    }
    if (body === undefined) {
        body = '<h2>' + button(title, source(event.titleKey)) + '</h2>' +
            (picture ? '<img alt="" src="' + pngDataUrl(picture) + '">' : '') +
            '<p>' + button(desc, source(event.descKey)) + '</p>' + event.options.map(o =>
                '<p>' + button(o.name?.value || o.nameKey, source(o.nameKey)) + '</p>').join('');
    }
    if (event.conditionalDesc) body += '<p>' + button(vscode.l10n.t('Conditional description — open script'), event.conditionalDesc) + '</p>';
    const nonce = randomUUID();
    const html = `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'nonce-${nonce}'">
<style>body{font-family:var(--vscode-font-family);max-width:564px;margin:1em auto;padding:0 1em}img{display:block;max-width:100%}
.window{position:relative}.window img{width:100%}p{white-space:pre-wrap}button{font:inherit;white-space:inherit;text-align:inherit;color:var(--vscode-textLink-foreground);background:none;border:0;padding:0;cursor:pointer}
button:hover{text-decoration:underline}.area{position:absolute;left:2%;width:96%;color:transparent;overflow:hidden}
.area:hover,.area:focus-visible{outline:2px solid var(--vscode-focusBorder)}</style></head><body>${body}
<script nonce="${nonce}">const vscode=acquireVsCodeApi();document.addEventListener('click',e=>{const b=e.target.closest('button');if(b)vscode.postMessage({generation:${generation},index:Number(b.dataset.index)});});</script></body></html>`;
    return { html, locations };
}

/** A single explicitly opened preview, pinned to an event identity across document edits. */
export class EventPreviewPanel {
    private panel?: vscode.WebviewPanel;
    private target?: PreviewTarget;
    private request?: vscode.CancellationTokenSource;
    private timer?: ReturnType<typeof setTimeout>;
    private locations: Location[] = [];
    private generation = 0;
    private dirty = false;
    private listeners: vscode.Disposable[] = [];

    constructor(private readonly client: () => LanguageClient | undefined) {}

    async show(target: PreviewTarget): Promise<void> {
        this.invalidate();
        this.target = target;
        if (!this.panel) {
            const panel = vscode.window.createWebviewPanel('paradoxcode.eventPreview', target.id,
                { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true }, { enableScripts: true, localResourceRoots: [] });
            this.panel = panel;
            panel.onDidDispose(() => this.clear());
            this.listeners.push(panel.onDidChangeViewState(() => {
                if (panel.visible && this.dirty) this.schedule();
            }), panel.webview.onDidReceiveMessage(async message => {
                if (message?.generation !== this.generation || !Number.isSafeInteger(message.index)) return;
                const loc = this.locations[message.index];
                if (!loc) return;
                const doc = await openTextDocumentForNavigation(vscode.Uri.parse(loc.uri));
                const { line, character } = loc.range.start;
                const position = new vscode.Position(line, character);
                await vscode.window.showTextDocument(doc, { viewColumn: vscode.window.visibleTextEditors.find(e => e.viewColumn !== panel.viewColumn)?.viewColumn
                    ?? vscode.ViewColumn.One, selection: new vscode.Range(position, position) });
            }), vscode.workspace.onDidChangeTextDocument(e => {
                if (e.contentChanges.length && (e.document.uri.toString() === this.target?.location.uri || e.document.languageId === 'localisation')) this.schedule();
            }));
        } else {
            this.panel.reveal(this.panel.viewColumn, true);
        }
        this.panel.title = target.id;
        this.panel.webview.html = escapeHtml(vscode.l10n.t('Loading event preview…'));
        await this.refresh();
    }

    private invalidate(): void {
        this.generation++;
        this.request?.cancel();
        clearTimeout(this.timer);
        this.locations = [];
    }

    private schedule(): void {
        this.invalidate();
        this.dirty = true;
        if (this.panel?.visible) this.timer = setTimeout(() => { void this.refresh(); }, 180);
    }

    private async refresh(): Promise<void> {
        const panel = this.panel;
        const target = this.target;
        if (!panel || !target) return;
        this.invalidate();
        this.dirty = false;
        const generation = this.generation;
        const request = new vscode.CancellationTokenSource();
        this.request = request;
        try {
            const client = this.client();
            if (!client) throw new Error(vscode.l10n.t('The ParadoxCode language server is not running.'));
            const document = await vscode.workspace.openTextDocument(vscode.Uri.parse(target.location.uri));
            if (request.token.isCancellationRequested) return;
            const result = await client.sendRequest('pdc/eventPreview', { textDocument: { uri: document.uri.toString() }, id: target.id }, request.token);
            if (request.token.isCancellationRequested) return;
            const card = parseHoverCardResponse(result)?.card;
            if (card?.kind !== 'event' || !card.event) throw new Error(vscode.l10n.t('Event {0} could not be found unambiguously in its source file.', target.id));
            const page = await eventPage(card, generation);
            if (generation !== this.generation) return;
            this.locations = page.locations;
            panel.webview.html = page.html;
        } catch (error) {
            if (generation === this.generation) panel.webview.html = escapeHtml(error instanceof Error ? error.message : String(error));
        } finally {
            if (this.request === request) this.request = undefined;
            request.dispose();
        }
    }

    private clear(): void {
        this.invalidate();
        this.panel = undefined;
        this.target = undefined;
        this.dirty = false;
        for (const listener of this.listeners.splice(0)) listener.dispose();
    }

    dispose(): void { this.panel?.dispose(); }
}
