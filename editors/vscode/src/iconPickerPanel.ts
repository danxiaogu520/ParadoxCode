// Mission-icon picker: a webview catalog of every sprite in the merged
// mod + vanilla index, filtered to mission icons by default. Picking a tile
// writes the sprite name into the editor the user came from — replacing the
// `icon = <value>` under the cursor, or inserting at the cursor — and closes
// the panel. The server stays text-only: the catalog comes from the shared
// client-side sprite index and pixels are decoded in the extension host,
// exactly like the mission preview.

import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { CataloguedSprite, isMissionIconSprite } from './gameAssets';
import { MissionPreviewPanel } from './previewPanel';
import { iconValueSpanAt } from './iconPickerInsert';

const PICKER_VIEW_TYPE = 'paradoxcode.missionIconPicker';

/** One catalog row shipped to the webview (metadata only; images stream later). */
interface SpriteWire {
    name: string;
    textureFile: string;
    frames?: number;
    origin: 'vanilla' | 'mod';
    /** Whether the mission-icon filter (name prefix ∪ texture directory) selects it. */
    mission: boolean;
}

/** Webview messages sent to the picker. */
type OutboundMessage =
    | { type: 'catalog'; sprites: SpriteWire[] }
    | { type: 'images'; textures: Record<string, string> }
    | { type: 'error'; message: string };

/** Webview messages received from the picker. */
type InboundMessage =
    | { type: 'requestImages'; names: string[] }
    | { type: 'insert'; name: string }
    | { type: 'copy'; name: string }
    | { type: 'close' };

export class MissionIconPickerPanel {
    private static panel: vscode.WebviewPanel | undefined;

    /** Closes the picker panel (after an insert, on Esc, or at deactivation). */
    public static dispose(): void {
        MissionIconPickerPanel.panel?.dispose();
        MissionIconPickerPanel.panel = undefined;
    }

    public static async show(extensionUri: vscode.Uri): Promise<void> {
        if (MissionIconPickerPanel.panel) {
            MissionIconPickerPanel.panel.reveal(vscode.ViewColumn.Beside, true);
            return;
        }
        const panel = vscode.window.createWebviewPanel(
            PICKER_VIEW_TYPE,
            'Mission Icons',
            vscode.ViewColumn.Beside,
            {
                enableScripts: true,
                localResourceRoots: [
                    vscode.Uri.joinPath(extensionUri, 'media'),
                ],
                retainContextWhenHidden: true,
            },
        );
        // Like the mission preview: open beside the source editor and keep
        // the editor focused, so the first click already has a target.
        panel.reveal(vscode.ViewColumn.Beside, true);
        MissionIconPickerPanel.panel = panel;
        panel.webview.html = MissionIconPickerPanel.html(panel.webview, extensionUri);
        panel.onDidDispose(() => {
            MissionIconPickerPanel.panel = undefined;
        });
        panel.webview.onDidReceiveMessage((message: InboundMessage) => {
            switch (message.type) {
                case 'requestImages':
                    void MissionIconPickerPanel.postImages(message.names);
                    return;
                case 'insert':
                    void MissionIconPickerPanel.insert(message.name);
                    return;
                case 'copy':
                    void vscode.env.clipboard.writeText(message.name);
                    return;
                case 'close':
                    MissionIconPickerPanel.dispose();
                    return;
            }
        });
        await MissionIconPickerPanel.postCatalog();
    }

    /** Ships the full sprite metadata; the webview filters, searches, and
     * asks for pixel batches as tiles scroll into view. */
    private static async postCatalog(): Promise<void> {
        const panel = MissionIconPickerPanel.panel;
        if (!panel) {
            return;
        }
        const store = MissionPreviewPanel.store();
        const catalog = [...store.spriteCatalog().values()]
            .sort((a, b) => a.name.localeCompare(b.name));
        if (catalog.length === 0) {
            MissionIconPickerPanel.post(panel, {
                type: 'error',
                message: 'No sprites were found. Point ParadoxCode at the EU4 installation '
                    + '(paradoxcode.gameDirectory) or open a mod workspace, then reopen the picker.',
            });
            return;
        }
        const sprites: SpriteWire[] = catalog.map((entry: CataloguedSprite) => ({
            name: entry.name,
            textureFile: entry.textureFile,
            ...(entry.frames !== undefined ? { frames: entry.frames } : {}),
            origin: entry.origin,
            mission: isMissionIconSprite(entry),
        }));
        MissionIconPickerPanel.post(panel, { type: 'catalog', sprites });
    }

    /** Decodes the requested sprite names (first frame only) and streams the
     * resulting data URLs back; the mtime-keyed caches make repeat batches cheap. */
    private static async postImages(names: string[]): Promise<void> {
        const panel = MissionIconPickerPanel.panel;
        if (!panel || names.length === 0) {
            return;
        }
        const store = MissionPreviewPanel.store();
        const previews = await store.spriteIconUrls(names.slice(0, 64));
        if (MissionIconPickerPanel.panel !== panel) {
            return;
        }
        const textures: Record<string, string> = {};
        for (const [name, preview] of Object.entries(previews)) {
            textures[name] = preview.url;
        }
        if (Object.keys(textures).length > 0) {
            MissionIconPickerPanel.post(panel, { type: 'images', textures });
        }
    }

    /**
     * Writes the chosen sprite name into the focused EU4 editor — replacing
     * the `icon` value under the cursor, else inserting at the cursor — and
     * closes the panel. With no EU4 editor focused the name is copied to the
     * clipboard so browse-only use still pays off.
     */
    private static async insert(name: string): Promise<void> {
        const editor = vscode.window.activeTextEditor;
        if (!editor || editor.document.languageId !== 'eu4') {
            await vscode.env.clipboard.writeText(name);
            void vscode.window.showInformationMessage(
                `ParadoxCode: no EU4 editor focused; copied "${name}" to the clipboard.`,
            );
            MissionIconPickerPanel.dispose();
            return;
        }
        const position = editor.selection.active;
        const line = editor.document.lineAt(position.line).text;
        const span = iconValueSpanAt(line, position.character, position.line);
        let applied: boolean;
        try {
            applied = await editor.edit((edit) => {
                if (span) {
                    edit.replace(new vscode.Range(span.line, span.start, span.line, span.end), name);
                } else {
                    edit.insert(position, name);
                }
            });
        } catch {
            applied = false;
        }
        if (!applied) {
            void vscode.window.showWarningMessage(
                `ParadoxCode: could not write "${name}" into ${path.basename(editor.document.uri.fsPath)}.`,
            );
            return;
        }
        if (span) {
            const end = new vscode.Position(span.line, span.start + name.length);
            editor.selection = new vscode.Selection(end, end);
        }
        MissionIconPickerPanel.dispose();
    }

    private static post(panel: vscode.WebviewPanel, message: OutboundMessage): void {
        void panel.webview.postMessage(message);
    }

    private static html(webview: vscode.Webview, extensionUri: vscode.Uri): string {
        const template = fs.readFileSync(
            path.join(extensionUri.fsPath, 'media', 'icon-picker.html'),
            'utf8',
        );
        const styleUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'icon-picker.css'),
        );
        const scriptUri = webview.asWebviewUri(
            vscode.Uri.joinPath(extensionUri, 'media', 'icon-picker.js'),
        );
        return template
            .replaceAll('{{cspSource}}', webview.cspSource)
            .replaceAll('{{styleUri}}', styleUri.toString())
            .replaceAll('{{scriptUri}}', scriptUri.toString());
    }
}
