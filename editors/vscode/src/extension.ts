import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    LogMessageNotification,
    ServerOptions,
    State,
} from 'vscode-languageclient/lib/node/main';

import { MissionPreviewPanel } from './previewPanel';
import { readSharedConfig } from './sharedConfig';
import { findExecutableOnPath } from './serverPath';

const EU4_LANGUAGE_ID = 'eu4';

/** Visible diagnostic trail: activation, binary resolution, server start. */
const log = vscode.window.createOutputChannel('ParadoxCode');

/** Always-visible server state: ● running / ◐ starting / ○ stopped. */
const statusBar = vscode.window.createStatusBarItem(
    'paradoxcode.status',
    vscode.StatusBarAlignment.Left,
    100,
);
statusBar.text = 'PDX ○';
statusBar.tooltip = 'ParadoxCode: pdx-ls not running';
statusBar.show();

let client: LanguageClient | undefined;

/** Debounce helper: `delay` ms after the last call, then run once. */
function debounce(fn: () => void, delay: number): () => void {
    let timer: NodeJS.Timeout | undefined;
    return () => {
        if (timer !== undefined) {
            clearTimeout(timer);
        }
        timer = setTimeout(fn, delay);
    };
}

function isEu4Document(document: vscode.TextDocument): boolean {
    return document.languageId === EU4_LANGUAGE_ID;
}

/** Maps `paradoxcode.*` settings onto the shared LSP initialization options
 * contract (same keys and semantics as the Zed extension's
 * `lsp.pdx-ls.initialization_options`). */
function readInitializationOptions(): Record<string, unknown> {
    const config = vscode.workspace.getConfiguration('paradoxcode');
    const options: Record<string, unknown> = {};
    for (const key of [
        'projectConfig',
        'modDirectory',
        'vanillaIndexCache',
        'dependencies',
        'gameDirectory',
    ] as const) {
        const value = config.get<unknown>(key);
        if (value !== undefined && value !== '') {
            options[key] = value;
        }
    }
    return options;
}

/** Result of resolving the pdx-ls launch command. `missingOnPath` is true when
 * the fallback `$PATH` lookup produced a bare name that is not resolvable, so
 * the start is expected to fail with ENOENT unless the OS resolves it another
 * way. */
interface ServerResolution {
    command: string;
    source: string;
    missingOnPath: boolean;
}

/** Resolves the pdx-ls binary and records where it came from, so a missing
 * server is never silent. Precedence: `paradoxcode.pdxLsPath` setting >
 * `.pdx/project.toml [server].binary` > `pdx-ls` on `$PATH`. */
function resolveServerCommand(): ServerResolution {
    const configuredPath = vscode.workspace
        .getConfiguration('paradoxcode')
        .get<string>('pdxLsPath', '');
    if (configuredPath) {
        return {
            command: configuredPath,
            source: 'setting paradoxcode.pdxLsPath',
            missingOnPath: false,
        };
    }
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    if (workspaceFolder) {
        try {
            const shared = readSharedConfig(workspaceFolder);
            if (shared.binary) {
                return {
                    command: shared.binary,
                    source: `${workspaceFolder.uri.fsPath}/.pdx/project.toml [server].binary`,
                    missingOnPath: false,
                };
            }
        } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            log.appendLine(`WARNING: ${message}`);
            void vscode.window.showWarningMessage(`ParadoxCode: ${message}`);
        }
    }
    return {
        command: 'pdx-ls',
        source: '$PATH (pdx-ls)',
        missingOnPath: findExecutableOnPath('pdx-ls') === undefined,
    };
}

function createClient(): LanguageClient {
    const { command, source, missingOnPath } = resolveServerCommand();
    log.appendLine(`pdx-ls binary: ${command} (from ${source})`);
    if (missingOnPath) {
        const message =
            'pdx-ls was not found on PATH, so the language server will fail to start. ' +
            'Set "paradoxcode.pdxLsPath" in settings, add [server].binary to a ' +
            '.pdx/project.toml in the workspace, or install pdx-ls on PATH.';
        log.appendLine(`WARNING: ${message}`);
        void vscode.window.showWarningMessage(`ParadoxCode: ${message}`);
    }
    const serverOptions: ServerOptions = { command };

    const clientOptions: LanguageClientOptions = {
        // Path patterns mirror the `filenamePatterns` contribution, so the
        // server serves mission files even when the editor language
        // assignment did not kick in (the server classifies by path anyway).
        documentSelector: [
            { language: EU4_LANGUAGE_ID },
            { pattern: '**/common/**/*.txt' },
            { pattern: '**/events/**/*.txt' },
            { pattern: '**/decisions/**/*.txt' },
            { pattern: '**/missions/**/*.txt' },
            { pattern: '**/history/**/*.txt' },
            { pattern: '**/interface/**/*.gui' },
            { pattern: '**/interface/**/*.gfx' },
        ],
        initializationOptions: readInitializationOptions(),
    };
    return new LanguageClient(
        'pdx-ls',
        'ParadoxCode Language Server',
        serverOptions,
        clientOptions,
    );
}

function updateStatus(state: State): void {
    switch (state) {
        case State.Running:
            statusBar.text = 'PDX ●';
            statusBar.tooltip = 'ParadoxCode: pdx-ls running';
            break;
        case State.Starting:
            statusBar.text = 'PDX ◐';
            statusBar.tooltip = 'ParadoxCode: pdx-ls starting…';
            break;
        default:
            statusBar.text = 'PDX ○';
            statusBar.tooltip = 'ParadoxCode: pdx-ls not running';
    }
}

function startClient(): void {
    try {
        client = createClient();
        client.onDidChangeState((event) => updateStatus(event.newState));
        // The server's `window/logMessage` trail (rules loading, workspace scan,
        // Vanilla cache build, readiness) is forwarded to the ParadoxCode output
        // channel and reflected in the status-bar tooltip, so "what is pdx-ls
        // doing" is visible without opening any panel.
        client.onNotification(LogMessageNotification.type, (params) => {
            log.appendLine(`[pdx-ls] ${params.message}`);
            statusBar.tooltip = `ParadoxCode: ${params.message}`;
        });
        updateStatus(client.state);
        client.start();
        log.appendLine('language server client started');
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR: ${message}`);
        void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
    }
}

function stopClient(): void {
    if (client) {
        const previous = client;
        client = undefined;
        void previous.stop();
        updateStatus(State.Stopped);
        log.appendLine('language server client stopped');
    }
}

export function activate(context: vscode.ExtensionContext): void {
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? 'none';
    log.appendLine(`ParadoxCode extension activated (workspace root: ${root})`);
    log.show(true);

    startClient();

    context.subscriptions.push(
        vscode.commands.registerCommand('paradoxcode.showMissionPreview', () => {
            MissionPreviewPanel.show(context.extensionUri);
        }),
        { dispose: () => MissionPreviewPanel.dispose() },
    );

    // Live refresh: any change to the active EU4 document re-renders the
    // preview from the server's `pdx/missionPreview` layout.
    const refresh = debounce(() => {
        void MissionPreviewPanel.refresh(client);
    }, 150);
    context.subscriptions.push(
        vscode.window.onDidChangeActiveTextEditor(() => refresh()),
        vscode.workspace.onDidChangeTextDocument((event) => {
            if (isEu4Document(event.document)) {
                refresh();
            }
        }),
        // Restart the client when the workspace root changes (e.g. the dev
        // host starts on `editors/vscode` and then opens the mod folder), so
        // the binary and initialization options follow the current workspace.
        vscode.workspace.onDidChangeWorkspaceFolders(() => {
            stopClient();
            startClient();
            void MissionPreviewPanel.refresh(client);
        }),
    );
}

export function deactivate(): void {
    stopClient();
}
