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
    start: number;
    end: number;
    /** UTF-16 LSP range supplied by pdx-ls. Kept separate from legacy byte spans. */
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
    start: number;
    end: number;
    sourceRange: SourceRange | null;
}

export interface MissionExternal {
    tree: number;
    mission: number;
    label: string;
}

export interface MissionPreview {
    /** URI/version of the document used to compute this payload. */
    documentUri?: string | null;
    documentVersion?: number | null;
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
    | { type: 'jump'; uri: string; range: SourceRange | null; start?: number; end?: number }
    | { type: 'openGroup'; uri: string; range: SourceRange | null; start?: number; end?: number }
    | { type: 'exportPng'; dataUri: string }
    | { type: 'exportJson'; json: string }
    | { type: 'exportSvg'; svg: string };

function isEu4Document(document: vscode.TextDocument): boolean {
    return document.languageId === EU4_LANGUAGE_ID;
}

/** Mission-file path patterns, mirroring the extension's `filenamePatterns`
 * and the server's scan whitelist. Used so the preview works even when the
 * editor language assignment did not kick in (e.g. the file opened before
 * the extension was loaded). `[\\/]` matches both separator styles. */
const MISSION_PATH_PATTERNS = [
    /[\\/]common[\\/].+\.txt$/i,
    /[\\/]events[\\/].+\.txt$/i,
    /[\\/]decisions[\\/].+\.txt$/i,
    /[\\/]missions[\\/].+\.txt$/i,
    /[\\/]history[\\/].+\.txt$/i,
    /[\\/]interface[\\/].+\.(gui|gfx)$/i,
];

function isPreviewDocument(document: vscode.TextDocument): boolean {
    return (
        document.languageId === EU4_LANGUAGE_ID ||
        MISSION_PATH_PATTERNS.some((pattern) => pattern.test(document.uri.fsPath))
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
                    void MissionPreviewPanel.jump(message.uri, message.range, message.start, message.end);
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
            payload.documentUri = payload.documentUri ?? documentUri;
            payload.documentVersion = payload.documentVersion ?? documentVersion;
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
        start?: number,
        end?: number,
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
        const range = sourceRange
            ? new vscode.Range(
                  new vscode.Position(sourceRange.start.line, sourceRange.start.character),
                  new vscode.Position(sourceRange.end.line, sourceRange.end.character),
              )
            : (() => {
                  // Compatibility fallback for older pdx-ls binaries.  New servers always send
                  // sourceRange, which is already UTF-16 and therefore safe for VS Code.
                  const text = editor.document.getText();
                  const safeStart = Math.min(start ?? 0, text.length);
                  const safeEnd = Math.min(Math.max(end ?? safeStart + 1, safeStart + 1), text.length);
                  return new vscode.Range(
                      editor.document.positionAt(safeStart),
                      editor.document.positionAt(safeEnd),
                  );
              })();
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
