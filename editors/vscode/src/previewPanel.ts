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
}

export interface MissionExternal {
    tree: number;
    mission: number;
    label: string;
}

export interface MissionPreview {
    nodes: MissionNode[];
    arrows: MissionArrow[];
    groups: MissionGroup[];
    external: MissionExternal[];
    diagnostics: { severity: number; code: string; message: string }[];
    /** Game sprite data URLs (`data:image/png;base64,`), keyed by sprite name. */
    textures: Record<string, string>;
}

/** Webview messages sent to the renderer. */
type OutboundMessage =
    | { type: 'preview'; payload: MissionPreview }
    | { type: 'empty'; message: string }
    | { type: 'error'; message: string };

/** Webview messages received from the renderer. */
type InboundMessage =
    | { type: 'jump'; start: number; end: number }
    | { type: 'openGroup'; start: number; end: number };

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
    /[\\/]localisation[\\/].+\.(yml|yaml)$/i,
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

    /** Closes the preview panel (extension deactivation). */
    public static dispose(): void {
        MissionPreviewPanel.panel?.dispose();
        MissionPreviewPanel.panel = undefined;
    }

    public static show(extensionUri: vscode.Uri): void {
        if (MissionPreviewPanel.panel) {
            MissionPreviewPanel.panel.reveal(vscode.ViewColumn.Beside, true);
            void MissionPreviewPanel.refresh();
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
        panel.onDidDispose(() => {
            MissionPreviewPanel.panel = undefined;
        });

        panel.webview.onDidReceiveMessage((message: InboundMessage) => {
            switch (message.type) {
                case 'jump':
                case 'openGroup':
                    MissionPreviewPanel.jump(message.start, message.end);
                    return;
            }
        });

        void MissionPreviewPanel.refresh();
    }

    /** Fetches the mission layout for the active EU4 document from pdx-ls and
     * pushes it to the webview. No-ops when the panel is closed. */
    public static async refresh(client?: LanguageClient): Promise<void> {
        const panel = MissionPreviewPanel.panel;
        if (!panel) {
            return;
        }
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
                { path: logical, text: editor.getText() },
            )) as MissionPreview;
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

    private static jump(start: number, end: number): void {
        const editor = vscode.window.activeTextEditor;
        if (!editor) {
            return;
        }
        const text = editor.document.getText();
        const range = new vscode.Range(
            editor.document.positionAt(Math.min(start, text.length)),
            editor.document.positionAt(Math.min(Math.max(end, start + 1), text.length)),
        );
        editor.selection = new vscode.Selection(range.start, range.end);
        editor.revealRange(range, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
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
