import { randomUUID } from 'node:crypto';
import * as vscode from 'vscode';
import type { Location } from 'vscode-languageserver-protocol';
import type { LanguageClient } from 'vscode-languageclient/node';
import type { TextureImageData } from './gameAssets';
import { EVENT_DESC_COLORS, isPreviewLocation, parseHoverCardResponse, parseLocFormat, type HoverCardWire } from './hoverCards';
import { MissionPreviewPanel } from './previewPanel';
import { openTextDocumentForNavigation } from './transparentLoc';

export interface PreviewTarget {
    kind: 'event' | 'mission';
    id: string;
    title?: string;
    titleSource?: { key: string; location: Location };
    missingLocalisation?: string[];
    language?: string;
    eventType?: string;
    sourceName?: string;
    prerequisiteIssues?: [string, boolean][];
    location: Location;
    path?: string;
}

export function parsePreviewTarget(value: unknown): PreviewTarget | undefined {
    const t = value as Partial<PreviewTarget> | undefined;
    if (!t || (t.kind !== 'event' && t.kind !== 'mission') || !t.id || typeof t.id !== 'string'
        || !isPreviewLocation(t.location) || !t.location.uri.startsWith('file:')) return undefined;
    const source = t.titleSource;
    const titleSource = source && typeof source.key === 'string' && isPreviewLocation(source.location)
        && /^(?:file|pdcloc):/.test(source.location.uri) ? source : undefined;
    const missingLocalisation = Array.isArray(t.missingLocalisation)
        ? t.missingLocalisation.filter(key => typeof key === 'string') : undefined;
    const prerequisiteIssues = Array.isArray(t.prerequisiteIssues)
        ? t.prerequisiteIssues.filter(issue => Array.isArray(issue) && typeof issue[0] === 'string'
            && typeof issue[1] === 'boolean') : undefined;
    return { kind: t.kind, id: t.id, title: typeof t.title === 'string' ? t.title : undefined, titleSource,
        language: typeof t.language === 'string' ? t.language : undefined, missingLocalisation, prerequisiteIssues,
        eventType: typeof t.eventType === 'string' ? t.eventType : undefined,
        sourceName: typeof t.sourceName === 'string' ? t.sourceName : undefined,
        location: { ...t.location, uri: vscode.Uri.parse(t.location.uri).toString() },
        path: typeof t.path === 'string' ? t.path : undefined };
}

function escapeHtml(text: string): string {
    return text.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

async function eventPage(card: HoverCardWire, generation: number, uri: string) {
    const event = card.event!;
    const store = MissionPreviewPanel.store();
    const assets = card.cardAssets ?? {};
    const title = parseLocFormat(event.title?.value || event.titleKey || event.id);
    const desc = parseLocFormat(event.desc?.value || event.descKey || '', EVENT_DESC_COLORS);
    const options = event.options.map(option => parseLocFormat(option.name?.value || option.nameKey));
    const tags = new Set<string>();
    for (const label of [title, desc, ...options]) {
        for (const { flag } of label.segments) {
            if (flag) tags.add(flag);
        }
    }
    const load = (asset?: HoverCardWire['asset']) => asset && !asset.archiveMember ? store.textureFile(asset.path) : undefined;
    const preferred = event.options.length <= 2 ? 0 : event.options.length <= 4 ? 1 : 2;
    const [top, middle, bottoms, optionButton, picture, flags] = await Promise.all([
        load(assets.backgroundTop), load(assets.backgroundMiddle),
        Promise.all([assets.backgroundBottomS, assets.backgroundBottomM, assets.backgroundBottomL].map(load)),
        load(assets.optionButton), load(card.asset),
        store.flagUrls(tags),
    ]);
    const bottom = bottoms[preferred] ?? bottoms.find(Boolean);
    const locations: Location[] = [];
    const button = (label: ReturnType<typeof parseLocFormat>, location?: Location) => {
        const text = label.segments.map(segment => {
            const flag = segment.flag && flags.get(segment.flag);
            if (flag) return `<img class="loc-flag" alt="${segment.text}" src="${flag}">`;
            const content = escapeHtml(segment.text);
            return segment.color ? `<span style="color:${segment.color}">${content}</span>` : content;
        }).join('');
        if (!location) return text;
        return `<button data-index="${locations.push(location) - 1}">${text}</button>`;
    };
    const source = (key?: string | null) => key && event.sources && Object.hasOwn(event.sources, key) ? event.sources[key] : undefined;
    // The original chrome extends beyond the 564px text window. Preserve its
    // scale and tile the middle behind the entire description and options.
    const chrome = (image: TextureImageData, repeat = 'no-repeat') =>
        `background:url('${image.url}') center top / ${100 * image.width / 564}% auto ${repeat}`;
    const cap = (image: TextureImageData) => `${chrome(image)};aspect-ratio:564/${image.height}`;
    const optionStyle = optionButton ? ` style="background:url('${optionButton.url}') center / 100% 100% no-repeat"` : '';
    const optionClass = optionButton ? 'event-option textured' : 'event-option';
    const eventKey = escapeHtml(JSON.stringify([uri, event.id]));
    const nonce = randomUUID();
    const html = `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'nonce-${nonce}'">
<style>
body{font-family:var(--vscode-font-family,sans-serif);max-width:564px;margin:1em auto;padding:0 1em}
.event-content{position:relative;isolation:isolate;display:flow-root}
.event-chrome{position:absolute;z-index:-1;inset:0;display:flex;flex-direction:column;pointer-events:none}
.chrome-top{flex:none}
.chrome-middle{flex:1}
.event-header{box-sizing:border-box;text-align:center;padding:8px 5% 0}
.event-header.textured{padding-top:6%;min-height:clamp(115px,39vw,223px);color:#fff;text-shadow:1px 1px #000}
h2{margin:0;line-height:1.15;font-size:clamp(18px,5vw,29px)}
.picture{display:block;max-width:100%;height:auto;margin:9px auto 0}
.event-desc{margin:7px 5.5% 14px;font-size:16px;line-height:1.35}
.event-desc.textured{color:#000}
.event-options{margin:0 1.5%;display:grid;gap:1px}
.event-option{padding:4px 8px;text-align:center;min-height:23px;line-height:1.35}
.event-option.textured{color:#fff;text-shadow:1px 1px #000}
h2,.event-desc,.event-option{white-space:pre-wrap;overflow-wrap:anywhere}
.loc-flag{display:inline-block;height:1em;width:auto;vertical-align:-.1em}
button{display:block;width:100%;color:inherit;font:inherit;white-space:inherit;overflow-wrap:inherit;text-align:inherit;
    background:none;border:0;padding:0;cursor:pointer}
button:hover{text-decoration:underline}
button:focus-visible{outline:2px solid var(--vscode-focusBorder)}
</style></head><body data-event="${eventKey}">
<article>
<div class="event-content">
<div class="event-chrome" aria-hidden="true">
${top ? `<div class="chrome-top" style="${cap(top)}"></div>` : ''}
${middle ? `<div class="chrome-middle" style="${chrome(middle, 'repeat-y')}"></div>` : ''}
</div>
<header class="event-header${top ? ' textured' : ''}">
<h2>${button(title, source(event.titleKey))}</h2>
${picture ? `<img class="picture" width="${picture.width}" height="${picture.height}" alt="" src="${picture.url}">` : ''}
</header>
<p class="event-desc${middle ? ' textured' : ''}">${button(desc, source(event.descKey))}</p>
<div class="event-options">${options.map((option, i) =>
        `<div class="${optionClass}"${optionStyle}>${button(option, source(event.options[i].nameKey))}</div>`).join('')}</div>
</div>
${bottom ? `<div aria-hidden="true" style="${cap(bottom)}"></div>` : ''}
</article>
${event.conditionalDesc ? `<p>${button(parseLocFormat(vscode.l10n.t('Conditional description — open script')), event.conditionalDesc)}</p>` : ''}
<script nonce="${nonce}">
const vscode=acquireVsCodeApi();
const eventKey=document.body.dataset.event;
const saved=vscode.getState();
if(saved?.eventKey===eventKey) window.scrollTo(0,saved.scrollTop);
window.addEventListener('scroll',()=>vscode.setState({eventKey,scrollTop:window.scrollY}),{passive:true});
document.addEventListener('click',e=>{
    const button=e.target.closest('button[data-index]');
    if(button) vscode.postMessage({generation:${generation},index:Number(button.dataset.index)});
});
</script></body></html>`;
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
            const page = await eventPage(card, generation, target.location.uri);
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
