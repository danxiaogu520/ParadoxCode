import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/node';

const PREVIEW_VIEW_TYPE = 'paradoxcode.missionPreview';

const EU4_LANGUAGE_ID = 'eu4';

/** Wire shape of `pdx/missionPreview` (see `pdx-lsp::requests`). */
export interface MissionNode {
    tree: number;
    mission: number;
    id: string;
    /** Mission icon sprite name (`icon = mission_x`), or null. */
    icon: string | null;
    /** Derived localisation key (`{id}_title`), always present. */
    titleKey: string;
    /** Resolved localised title, or `null` when no active definition exists. */
    title: { language: string | null; value: string } | null;
    x: number;
    y: number;
    /** UTF-16 LSP range supplied by pdx-ls. */
    sourceRange: SourceRange | null;
    isRoot: boolean;
    hasError: boolean;
    hasWarning: boolean;
}

export interface MissionArrow {
    glyph: string;
    /** Game sprite name for this glyph, or null when unavailable. */
    texture: string | null;
    x: number;
    y: number;
}

export interface MissionGroup {
    tree: number;
    label: string;
    x: number;
    y: number;
    sourceRange: SourceRange | null;
}

export interface MissionExternal {
    tree: number;
    mission: number;
    label: string;
}

export interface MissionPreview {
    /** URI/version of the document used to compute this payload. */
    documentUri: string;
    documentVersion: number;
    nodes: MissionNode[];
    arrows: MissionArrow[];
    groups: MissionGroup[];
    external: MissionExternal[];
    diagnostics: { severity: number; code: string; message: string }[];
    /** Game sprite data URLs (`data:image/png;base64,`), keyed by sprite name. */
    textures: Record<string, string>;
}

export interface SourcePosition {
    line: number;
    character: number;
}

export interface SourceRange {
    start: SourcePosition;
    end: SourcePosition;
}

/** Webview messages sent to the renderer. */
type OutboundMessage =
    | { type: 'preview'; payload: MissionPreview }
    | { type: 'empty'; message: string }
    | { type: 'error'; message: string }
    | { type: 'options'; zoomSensitivity: number; showTextures: boolean };

/** Webview messages received from the renderer. */
type InboundMessage =
    | { type: 'jump'; uri: string; range: SourceRange | null }
    | { type: 'openGroup'; uri: string; range: SourceRange | null }
    | { type: 'exportPng'; dataUri: string }
    | { type: 'exportJson'; json: string }
    | { type: 'exportSvg'; svg: string };

function isEu4Document(document: vscode.TextDocument): boolean {
    return document.languageId === EU4_LANGUAGE_ID;
}

/** EU4 script path patterns, mirroring the extension's `filenamePatterns` and
 * the server's scan whitelist. Used so the preview works even when the editor
 * language assignment did not kick in (e.g. the file opened before the
 * extension was loaded). `[\\/]` matches both separator styles. */
const SCRIPT_PATH_PATTERNS = [
    /[\\/]common[\\/]achievements\.txt$/i,
    /[\\/]common[\\/]alerts\.txt$/i,
    /[\\/]common[\\/]graphicalculturetype\.txt$/i,
    /[\\/]common[\\/]historial_lucky\.txt$/i,
    /[\\/]common[\\/]technology\.txt$/i,
    /[\\/]common[\\/]advisortypes[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ages[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ai_army[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ai_attitudes[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ai_personalities[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ancestor_personalities[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]bookmarks[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]buildings[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]cb_types[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]centers_of_trade[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]church_aspects[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]client_states[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]colonial_regions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]countries[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]country_colors[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]country_tags[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]cultures[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]custom_country_colors[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]custom_gui[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]custom_ideas[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]decrees[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]defender_of_faith[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]defines[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]diplomatic_actions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]disasters[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]dynasty_colors[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]estate_agendas[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]estate_crown_land[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]estate_privileges[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]estates[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]estates_preload[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]event_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]factions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]federation_advancements[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]fervor[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]fetishist_cults[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]flagship_modifications[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]golden_bulls[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]government_mechanics[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]government_names[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]government_ranks[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]government_reforms[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]governments[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]great_projects[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]hegemons[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]holy_orders[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ideas[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]imperial_incidents[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]imperial_reforms[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]incidents[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]institutions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]insults[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]isolationism[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]leader_personalities[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]mercenary_companies[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]natives[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]naval_doctrines[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]new_diplomatic_actions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]on_actions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]opinion_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]parliament_bribes[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]parliament_issues[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]peace_treaties[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]personal_deities[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]policies[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]powerprojection[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]prices[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]professionalism[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]province_names[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]province_triggered_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]rebel_types[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]region_colors[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]religions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]religious_conversions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]religious_reforms[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]revolt_triggers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]revolution[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]ruler_personalities[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]scripted_effects[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]scripted_functions[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]scripted_triggers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]state_edicts[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]static_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]subject_type_upgrades[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]subject_types[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]technologies[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]timed_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]trade_companies[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]tradecompany_investments[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]tradegoods[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]tradenodes[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]trading_policies[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]triggered_modifiers[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]units[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]units_display[\\/][^\\/]+\.txt$/i,
    /[\\/]common[\\/]wargoal_types[\\/][^\\/]+\.txt$/i,
    /[\\/]customizable_localization[\\/][^\\/]+\.txt$/i,
    /[\\/]decisions[\\/][^\\/]+\.txt$/i,
    /[\\/]events[\\/][^\\/]+\.txt$/i,
    /[\\/]hints[\\/][^\\/]+\.txt$/i,
    /[\\/]history[\\/](advisors|countries|diplomacy|provinces|wars)[\\/][^\\/]+\.txt$/i,
    /[\\/]map[\\/]ambient_object\.txt$/i,
    /[\\/]map[\\/]area\.txt$/i,
    /[\\/]map[\\/]climate\.txt$/i,
    /[\\/]map[\\/]continent\.txt$/i,
    /[\\/]map[\\/]lakes[\\/]00_lakes\.txt$/i,
    /[\\/]map[\\/]positions\.txt$/i,
    /[\\/]map[\\/]provincegroup\.txt$/i,
    /[\\/]map[\\/]random[\\/]RNWScenarios\.txt$/i,
    /[\\/]map[\\/]random[\\/]RandomLakeNames\.txt$/i,
    /[\\/]map[\\/]random[\\/]RandomLandNames\.txt$/i,
    /[\\/]map[\\/]random[\\/]RandomSeaNames\.txt$/i,
    /[\\/]map[\\/]region\.txt$/i,
    /[\\/]map[\\/]seasons\.txt$/i,
    /[\\/]map[\\/]superregion\.txt$/i,
    /[\\/]map[\\/]terrain\.txt$/i,
    /[\\/]map[\\/]trade_winds\.txt$/i,
    /[\\/]music[\\/][^\\/]+\.txt$/i,
    /[\\/]missions[\\/][^\\/]+\.txt$/i,
    /[\\/]sound[\\/][^\\/]+\.txt$/i,
    /[\\/]sound[\\/]amb[\\/][^\\/]+\.txt$/i,
    /[\\/]sound[\\/]battle[\\/][^\\/]+\.txt$/i,
    /[\\/]sound[\\/]battle[\\/]naval[\\/][^\\/]+\.txt$/i,
    /[\\/]tutorial[\\/][^\\/]+\.txt$/i,
    /[\\/]gfx[\\/][^\\/]+\.txt$/i,
    /[\\/]gfx[\\/]combat_result[\\/][^\\/]+\.txt$/i,
    /[\\/]gfx[\\/]sprite_packs[\\/][^\\/]+\.txt$/i,
    /[\\/]gfx[\\/]sprite_packs_order[\\/][^\\/]+\.txt$/i,
    /[\\/]interface[\\/][^\\/]+\.(txt|gui|gfx)$/i,
    /[\\/]interface[\\/]assets[\\/][^\\/]+\.gfx$/i,
    /[\\/]interface[\\/]government_mechanics[\\/][^\\/]+\.(txt|gui|gfx)$/i,
    /[\\/]interface[\\/]state_view[\\/][^\\/]+\.txt$/i,
];

function isPreviewDocument(document: vscode.TextDocument): boolean {
    return (
        document.languageId === EU4_LANGUAGE_ID ||
        SCRIPT_PATH_PATTERNS.some((pattern) => pattern.test(document.uri.fsPath))
    );
}

/** The logical path expected by the server, relative to the workspace root
 * (the same convention `pdx/classifyPaths` uses). */
function logicalPath(document: vscode.TextDocument): string | undefined {
    const workspace = vscode.workspace.getWorkspaceFolder(document.uri);
    if (!workspace) {
        return undefined;
    }
    const relative = path.posix.relative(workspace.uri.path, document.uri.path);
    if (relative.startsWith('..')) {
        return undefined;
    }
    return relative;
}

export class MissionPreviewPanel {
    private static panel: vscode.WebviewPanel | undefined;
    private static requestSequence = 0;
    private static previewUri: string | undefined;
    private static previewVersion: number | undefined;

    /** Closes the preview panel (extension deactivation). */
    public static dispose(): void {
        MissionPreviewPanel.panel?.dispose();
        MissionPreviewPanel.panel = undefined;
        MissionPreviewPanel.requestSequence += 1;
        MissionPreviewPanel.previewUri = undefined;
        MissionPreviewPanel.previewVersion = undefined;
    }

    public static show(extensionUri: vscode.Uri, client?: LanguageClient): void {
        if (MissionPreviewPanel.panel) {
            MissionPreviewPanel.panel.reveal(vscode.ViewColumn.Beside, true);
            void MissionPreviewPanel.refresh(client);
            return;
        }

        const panel = vscode.window.createWebviewPanel(
            PREVIEW_VIEW_TYPE,
            'Mission Tree Preview',
            vscode.ViewColumn.Beside,
            {
                enableScripts: true,
                localResourceRoots: [
                    vscode.Uri.joinPath(extensionUri, 'media'),
                ],
                retainContextWhenHidden: true,
            },
        );
        // Like Markdown's "Open Preview to the Side": always open in a new
        // column next to the source editor and keep focus on the editor.
        panel.reveal(vscode.ViewColumn.Beside, true);
        MissionPreviewPanel.panel = panel;
        panel.webview.html = MissionPreviewPanel.html(panel.webview, extensionUri);
        MissionPreviewPanel.postOptions(panel);
        panel.onDidDispose(() => {
            MissionPreviewPanel.panel = undefined;
        });

        panel.webview.onDidReceiveMessage((message: InboundMessage) => {
            switch (message.type) {
                case 'jump':
                case 'openGroup':
                    void MissionPreviewPanel.jump(message.uri, message.range);
                    return;
                case 'exportPng':
                    void MissionPreviewPanel.exportPng(message.dataUri);
                    return;
                case 'exportJson':
                    void MissionPreviewPanel.exportJson(message.json);
                    return;
                case 'exportSvg':
                    void MissionPreviewPanel.exportSvg(message.svg);
                    return;
            }
        });

        void MissionPreviewPanel.refresh(client);
    }

    /** Fetches the mission layout for the active EU4 document from pdx-ls and
     * pushes it to the webview. No-ops when the panel is closed. */
    public static async refresh(client?: LanguageClient): Promise<void> {
        const panel = MissionPreviewPanel.panel;
        if (!panel) {
            return;
        }
        MissionPreviewPanel.postOptions(panel);
        const requestId = ++MissionPreviewPanel.requestSequence;
        // The active editor document, falling back to the first open
        // mission-like document (the webview panel itself can steal focus).
        const active = vscode.window.activeTextEditor;
        const editor = active
            ? active.document
            : vscode.workspace.textDocuments.find((document) =>
                  isPreviewDocument(document),
              );
        if (!editor || !isPreviewDocument(editor)) {
            MissionPreviewPanel.post(panel, {
                type: 'empty',
                message:
                    'Open an EU4 mission file to preview its mission tree.\n\n' +
                    `active: ${editor ? editor.uri.fsPath : 'no open document'} (language: ${editor ? editor.languageId : 'n/a'})`,
            });
            return;
        }
        const documentUri = editor.uri.toString();
        const documentVersion = editor.version;
        const logical = logicalPath(editor);
        if (!logical) {
            MissionPreviewPanel.post(panel, {
                type: 'error',
                message: 'The mission file must live inside the workspace root.',
            });
            return;
        }
        if (!client) {
            MissionPreviewPanel.post(panel, {
                type: 'error',
                message: 'The ParadoxCode language server is not running.',
            });
            return;
        }
        try {
            const payload = (await client.sendRequest(
                'pdx/missionPreview',
                {
                    path: logical,
                    text: editor.getText(),
                    uri: documentUri,
                    version: documentVersion,
                },
            )) as MissionPreview;
            // A response is only useful when it still describes the document/version that was
            // captured.  This prevents a slow preview request from repainting a newer edit.
            if (requestId !== MissionPreviewPanel.requestSequence || panel !== MissionPreviewPanel.panel) {
                return;
            }
            const current = vscode.workspace.textDocuments.find(
                (document) => document.uri.toString() === documentUri,
            );
            if (!current || current.version !== documentVersion) {
                return;
            }
            MissionPreviewPanel.previewUri = documentUri;
            MissionPreviewPanel.previewVersion = documentVersion;
            MissionPreviewPanel.post(panel, { type: 'preview', payload });
        } catch (error) {
            MissionPreviewPanel.post(panel, {
                type: 'error',
                message: error instanceof Error ? error.message : String(error),
            });
        }
    }

    private static post(panel: vscode.WebviewPanel, message: OutboundMessage): void {
        void panel.webview.postMessage(message);
    }

    private static postOptions(panel: vscode.WebviewPanel): void {
        const config = vscode.workspace.getConfiguration('paradoxcode.preview');
        MissionPreviewPanel.post(panel, {
            type: 'options',
            zoomSensitivity: config.get<number>('zoomSensitivity', 1),
            showTextures: config.get<boolean>('showTextures', true),
        });
    }

    private static async jump(
        uri: string,
        sourceRange: SourceRange | null,
    ): Promise<void> {
        if (uri && MissionPreviewPanel.previewUri && uri !== MissionPreviewPanel.previewUri) {
            return;
        }
        let editor = vscode.window.visibleTextEditors.find(
            (candidate) => candidate.document.uri.toString() === uri,
        );
        if (!editor) {
            try {
                const document = await vscode.workspace.openTextDocument(vscode.Uri.parse(uri));
                editor = await vscode.window.showTextDocument(document, vscode.ViewColumn.One, false);
            } catch (error) {
                const message = error instanceof Error ? error.message : String(error);
                void vscode.window.showWarningMessage(`ParadoxCode: could not open mission source: ${message}`);
                return;
            }
        }
        if (MissionPreviewPanel.previewVersion !== undefined && editor.document.version !== MissionPreviewPanel.previewVersion) {
            void vscode.window.showInformationMessage(
                'ParadoxCode: the mission preview is out of date; edit refresh is still pending.',
            );
            return;
        }
        if (!sourceRange) {
            void vscode.window.showWarningMessage(
                'ParadoxCode: the language server did not return a UTF-16 source range for this item.',
            );
            return;
        }
        const range = new vscode.Range(
            new vscode.Position(sourceRange.start.line, sourceRange.start.character),
            new vscode.Position(sourceRange.end.line, sourceRange.end.character),
        );
        editor.selection = new vscode.Selection(range.start, range.end);
        editor.revealRange(range, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
    }

    private static async exportPng(dataUri: string): Promise<void> {
        const match = /^data:image\/png;base64,(.+)$/s.exec(dataUri);
        if (!match) {
            void vscode.window.showErrorMessage('ParadoxCode: the preview did not return a PNG image.');
            return;
        }
        const target = await vscode.window.showSaveDialog({
            saveLabel: 'Export Mission Tree PNG',
            filters: { 'PNG image': ['png'] },
            defaultUri: vscode.Uri.file(path.join(
                vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? process.cwd(),
                'mission-tree.png',
            )),
        });
        if (!target) {
            return;
        }
        await vscode.workspace.fs.writeFile(target, Buffer.from(match[1], 'base64'));
    }

    private static async exportJson(json: string): Promise<void> {
        const target = await vscode.window.showSaveDialog({
            saveLabel: 'Export Mission Tree JSON',
            filters: { JSON: ['json'] },
            defaultUri: vscode.Uri.file(path.join(
                vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? process.cwd(),
                'mission-tree.json',
            )),
        });
        if (!target) {
            return;
        }
        await vscode.workspace.fs.writeFile(target, Buffer.from(json, 'utf8'));
    }

    private static async exportSvg(svg: string): Promise<void> {
        const target = await vscode.window.showSaveDialog({
            saveLabel: 'Export Mission Tree SVG',
            filters: { 'SVG image': ['svg'] },
            defaultUri: vscode.Uri.file(path.join(
                vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? process.cwd(),
                'mission-tree.svg',
            )),
        });
        if (!target) {
            return;
        }
        await vscode.workspace.fs.writeFile(target, Buffer.from(svg, 'utf8'));
    }

    private static html(webview: vscode.Webview, extensionUri: vscode.Uri): string {
        const template = fs.readFileSync(
            path.join(extensionUri.fsPath, 'media', 'index.html'),
            'utf8',
        );
        const styleUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'style.css'),
        );
        const scriptUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'renderer.js'),
        );
        return template
            .replaceAll('{{cspSource}}', webview.cspSource)
            .replaceAll('{{styleUri}}', styleUri.toString())
            .replaceAll('{{scriptUri}}', scriptUri.toString());
    }
}
