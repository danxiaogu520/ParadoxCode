import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/node';
import { FRAME_SPRITE, FontAssets, GameAssetStore, findChineseFontMod, findGameDirectory } from './gameAssets';

const PREVIEW_VIEW_TYPE = 'paradoxcode.missionPreview';

/** Wire shape of `pdc/missionPreview` (see `pdc::requests`). */
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
    /** UTF-16 LSP range supplied by pdc. */
    sourceRange: SourceRange | null;
    hasError: boolean;
    hasWarning: boolean;
}

export interface MissionArrow {
    glyph: string;
    /** Game sprite name for this glyph, or null when unavailable. */
    texture: string | null;
    /** Tree (series) of the dependent mission the run points into. */
    tree: number;
    /** Tree (series) of the prerequisite mission the run starts from. */
    from: number;
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
    /** Sprite pixels and game fonts the renderer asked for, decoded client-side. */
    | { type: 'assets'; textures: Record<string, string>; fonts?: FontAssets }
    | {
        type: 'options';
        zoomSensitivity: number;
        showTextures: boolean;
        showExternalPrerequisites: boolean;
        showDiagnostics: boolean;
        gameFonts: boolean;
    };

/** Webview messages received from the renderer. */
type InboundMessage =
    | { type: 'jump'; uri: string; range: SourceRange | null }
    | { type: 'openGroup'; uri: string; range: SourceRange | null };

/** Mission-tree preview is only meaningful for `.txt` files inside a
 * `missions` folder — the game only loads mission trees from there, and the
 * server parses any top-level block as a series, so rendering other script
 * files produces pseudo trees. Path-based, not language-based, so the preview
 * still works when the editor language assignment did not kick in (e.g. the
 * file opened before the extension was loaded). `[\\/]` matches both
 * separator styles; `common/missions/` also ends with a `/missions/`
 * segment. */
const MISSION_PATH_PATTERN = /[\\/]missions[\\/][^\\/]+\.txt$/i;

function isPreviewDocument(document: vscode.TextDocument): boolean {
    return MISSION_PATH_PATTERN.test(document.uri.fsPath);
}

/** The logical path expected by the server, relative to the workspace root
 * (the same convention `pdc/classifyPaths` uses). */
function logicalPath(document: vscode.TextDocument): string | undefined {
    const workspace = vscode.workspace.getWorkspaceFolder(document.uri);
    if (!workspace) {
        return undefined;
    }
    // fsPath is the decoded, native-spelling form; the URI path component
    // would deliver percent-encoded segments (`%20`, `%3A`) the server would
    // then see as literal directory names.
    const relative = path
        .relative(workspace.uri.fsPath, document.uri.fsPath)
        .split(path.sep)
        .join('/');
    if (relative.startsWith('..') || path.isAbsolute(relative)) {
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
            }
        });

        void MissionPreviewPanel.refresh(client);
    }

    /** Fetches the mission layout for the active EU4 document from pdc and
     * pushes it to the webview. No-ops when the panel is closed. */
    public static async refresh(client?: LanguageClient): Promise<void> {
        const panel = MissionPreviewPanel.panel;
        if (!panel) {
            return;
        }
        MissionPreviewPanel.postOptions(panel);
        const requestId = ++MissionPreviewPanel.requestSequence;
        // The active editor document, falling back to the document behind the
        // last pushed preview: the webview panel itself can steal focus (e.g.
        // while the user pans the canvas), and picking any other open document
        // would silently re-target the preview.
        const active = vscode.window.activeTextEditor;
        const editor = active
            ? active.document
            : vscode.workspace.textDocuments.find(
                  (document) =>
                      document.uri.toString() === MissionPreviewPanel.previewUri,
              );
        if (!editor || !isPreviewDocument(editor)) {
            MissionPreviewPanel.post(panel, {
                type: 'empty',
                message: editor
                    ? 'Mission tree preview only renders .txt files inside a missions folder.\n\n' +
                      `active: ${editor.uri.fsPath}`
                    : 'Open a .txt file inside a missions folder and focus it to preview its mission tree.',
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
                'pdc/missionPreview',
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
            void MissionPreviewPanel.postAssets(panel, payload);
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

    /** Client-side asset store, rebuilt when its source directories change. */
    private static assetStore: GameAssetStore | undefined;
    private static assetStoreKey: string | undefined;
    /** Fonts are large payloads: post them once per store generation and panel. */
    private static postedFonts: { panel: vscode.WebviewPanel; key: string } | undefined;
    /** Sprites are incremental payloads too: the names the receiving panel
     * has already been sent. A rebuilt webview starts empty while this side's
     * warm cache would suppress resends as already delivered, so any wanted
     * name the panel has not received yet — first use, a rebuild after
     * close/reopen, or a sprite decoded earlier for a previous panel — is
     * force-resent. Store rebuilds need no tracking here: a fresh store is
     * cold and therefore ships everything anyway. */
    private static postedSprites: { panel: vscode.WebviewPanel; names: Set<string> } | undefined;

    /** Builds the shared client-side asset store, rebuilt when its source
     * directories change. Shared with the hover texture preview middleware. */
    public static store(): GameAssetStore {
        const config = vscode.workspace.getConfiguration('paradoxcode');
        const gameDirectory = findGameDirectory(config.get<string>('gameDirectory', '')) ?? '';
        const gameFonts = config.get<boolean>('preview.gameFonts', true);
        // The Chinese bitmap font is not shipped with the game: resolve the
        // mod that remaps vic_18 (setting override > newest workshop match).
        const chineseFontDirectory = gameFonts
            ? findChineseFontMod(gameDirectory || undefined, config.get<string>('preview.chineseFontMod', ''))
            : undefined;
        // The mod side of the texture lookup: an explicit mod directory,
        // else the workspace folders — the same roots the server treats as
        // the Current Mod source root.
        const modRoots: string[] = [];
        const modDirectory = config.get<string>('modDirectory', '');
        if (modDirectory.trim() !== '') {
            modRoots.push(modDirectory);
        }
        for (const folder of vscode.workspace.workspaceFolders ?? []) {
            modRoots.push(folder.uri.fsPath);
        }
        const uniqueModRoots = [...new Set(modRoots)];
        const key = `${gameDirectory}\0${chineseFontDirectory ?? ''}\0${uniqueModRoots.join('\0')}`;
        if (!MissionPreviewPanel.assetStore || MissionPreviewPanel.assetStoreKey !== key) {
            MissionPreviewPanel.assetStore = new GameAssetStore(
                gameDirectory || undefined,
                chineseFontDirectory,
                uniqueModRoots,
            );
            MissionPreviewPanel.assetStoreKey = key;
        }
        return MissionPreviewPanel.assetStore;
    }

    /** Decodes the sprites a payload references (frame, node icons, arrow
     * tiles) and pushes any newly loaded ones to the webview. Client-side
     * decoding keeps every per-keystroke `missionPreview` response pure text.
     * Names this panel has not received yet are resent from the warm cache
     * (see `postedSprites`). */
    private static async postAssets(
        panel: vscode.WebviewPanel,
        payload: MissionPreview,
    ): Promise<void> {
        const wanted = new Set<string>([FRAME_SPRITE]);
        for (const node of payload.nodes) {
            if (node.icon) {
                wanted.add(node.icon);
            }
        }
        for (const arrow of payload.arrows) {
            if (arrow.texture) {
                wanted.add(arrow.texture);
            }
        }
        const store = MissionPreviewPanel.store();
        const delivered = MissionPreviewPanel.postedSprites?.panel === panel
            ? MissionPreviewPanel.postedSprites.names
            : new Set<string>();
        const force = new Set<string>([...wanted].filter((name) => !delivered.has(name)));
        const textures = await store.spriteUrls([...wanted], force);
        const names = new Set<string>(delivered);
        for (const name of Object.keys(textures)) {
            names.add(name);
        }
        MissionPreviewPanel.postedSprites = { panel, names };
        const config = vscode.workspace.getConfiguration('paradoxcode');
        const fontsKey = MissionPreviewPanel.assetStoreKey ?? '';
        let fonts: FontAssets | undefined;
        if (
            config.get<boolean>('preview.gameFonts', true) &&
            (!MissionPreviewPanel.postedFonts ||
                MissionPreviewPanel.postedFonts.panel !== panel ||
                MissionPreviewPanel.postedFonts.key !== fontsKey)
        ) {
            const loaded = await store.loadFonts();
            MissionPreviewPanel.postedFonts = { panel, key: fontsKey };
            if (loaded.english || loaded.chinese) {
                fonts = loaded;
            }
        }
        if ((Object.keys(textures).length === 0 && !fonts) || MissionPreviewPanel.panel !== panel) {
            return;
        }
        MissionPreviewPanel.post(panel, { type: 'assets', textures, fonts });
    }

    private static postOptions(panel: vscode.WebviewPanel): void {
        const config = vscode.workspace.getConfiguration('paradoxcode.preview');
        MissionPreviewPanel.post(panel, {
            type: 'options',
            zoomSensitivity: config.get<number>('zoomSensitivity', 1),
            showTextures: config.get<boolean>('showTextures', true),
            showExternalPrerequisites: config.get<boolean>('showExternalPrerequisites', true),
            showDiagnostics: config.get<boolean>('showDiagnostics', true),
            gameFonts: config.get<boolean>('gameFonts', true),
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

    private static html(webview: vscode.Webview, extensionUri: vscode.Uri): string {
        const template = fs.readFileSync(
            path.join(extensionUri.fsPath, 'media', 'index.html'),
            'utf8',
        );
        const styleUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'style.css'),
        );
        const locFormatUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'loc-format.js'),
        );
        const scriptUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'renderer.js'),
        );
        return template
            .replaceAll('{{cspSource}}', webview.cspSource)
            .replaceAll('{{styleUri}}', styleUri.toString())
            .replaceAll('{{locFormatUri}}', locFormatUri.toString())
            .replaceAll('{{scriptUri}}', scriptUri.toString());
    }
}
