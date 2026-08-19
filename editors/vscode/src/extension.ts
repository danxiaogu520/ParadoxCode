import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    LogMessageNotification,
    RevealOutputChannelOn,
    ServerOptions,
    State,
} from 'vscode-languageclient/lib/node/main';

import { LoadedFilesProvider } from './fileExplorer';
import { MissionPreviewPanel } from './previewPanel';
import { readSharedConfig } from './sharedConfig';
import { findExecutableOnPath } from './serverPath';
import {
    DEFAULT_SERVER_REPOSITORY,
    DEFAULT_SERVER_VERSION,
    cachedServerPath,
    defaultInstallDirectory,
    installPdxLs,
} from './serverInstaller';

const EU4_LANGUAGE_ID = 'eu4';
const SERVER_SETTING_KEYS = [
    'pdxLsPath',
    'projectConfig',
    'modDirectory',
    'vanillaIndexCache',
    'dependencies',
    'gameDirectory',
    'serverVersion',
    'serverInstallDirectory',
] as const;

/** Visible diagnostic trail: activation, binary resolution, server start. */
const log = vscode.window.createOutputChannel('ParadoxCode');

/** Server state is deliberately compact so it works in narrow status bars. */
const statusBar = vscode.window.createStatusBarItem(
    'paradoxcode.status',
    vscode.StatusBarAlignment.Left,
    100,
);
statusBar.name = 'ParadoxCode Language Server';
statusBar.command = 'paradoxcode.openOutput';
statusBar.text = 'PDX ○';
statusBar.tooltip = 'ParadoxCode: pdx-ls not running';

let client: LanguageClient | undefined;
let missingServerWarningShown = false;
let extensionContext: vscode.ExtensionContext | undefined;

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

function isMissionDocument(document: vscode.TextDocument | undefined): boolean {
    if (!document || !isEu4Document(document)) {
        return false;
    }
    return /[\\/]common[\\/]missions[\\/].+\.txt$/i.test(document.uri.fsPath)
        || /[\\/]missions[\\/].+\.txt$/i.test(document.uri.fsPath);
}

function updateMissionContext(document: vscode.TextDocument | undefined): void {
    void vscode.commands.executeCommand('setContext', 'paradoxcodeMissionFile', isMissionDocument(document));
}

/** Maps `paradoxcode.*` settings onto the shared LSP initialization options contract. */
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

interface ServerResolution {
    command: string;
    source: string;
    missingOnPath: boolean;
}

/** Resolves the pdx-ls binary. Explicit user/workspace configuration always wins over the
 * optional downloaded cache and PATH fallback. */
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
    if (extensionContext) {
        const config = vscode.workspace.getConfiguration('paradoxcode');
        const cached = cachedServerPath(extensionContext, {
            version: config.get<string>('serverVersion', DEFAULT_SERVER_VERSION),
            installDirectory: config.get<string>('serverInstallDirectory', '') || defaultInstallDirectory(extensionContext),
        });
        if (cached) {
            return {
                command: cached,
                source: 'ParadoxCode checksum-verified server cache',
                missingOnPath: false,
            };
        }
    }
    return {
        command: 'pdx-ls',
        source: '$PATH (pdx-ls)',
        missingOnPath: findExecutableOnPath('pdx-ls') === undefined,
    };
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

function diagnosticIgnorePatterns(): string[] {
    return vscode.workspace
        .getConfiguration('paradoxcode')
        .get<string[]>('diagnosticIgnoreFiles', [])
        .filter((value): value is string => typeof value === 'string' && value.length > 0);
}

function relativeDiagnosticPath(uri: vscode.Uri): string {
    const folder = vscode.workspace.getWorkspaceFolder(uri);
    if (!folder) {
        return uri.fsPath.replace(/\\/g, '/');
    }
    return pathRelative(folder.uri.fsPath, uri.fsPath);
}

function pathRelative(root: string, file: string): string {
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/$/, '');
    const normalizedFile = file.replace(/\\/g, '/');
    return normalizedFile.startsWith(`${normalizedRoot}/`)
        ? normalizedFile.slice(normalizedRoot.length + 1)
        : normalizedFile;
}

function diagnosticsMiddleware(): NonNullable<LanguageClientOptions['middleware']> {
    return {
        handleDiagnostics(uri, diagnostics, next) {
            const config = vscode.workspace.getConfiguration('paradoxcode');
            const ignoredCodes = new Set(
                config
                    .get<string[]>('diagnosticIgnoreCodes', [])
                    .filter((value): value is string => typeof value === 'string'),
            );
            const patterns = diagnosticIgnorePatterns().map(globToRegExp);
            const relative = relativeDiagnosticPath(uri);
            const filtered = diagnostics.filter((diagnostic) => {
                const code = diagnostic.code === undefined ? undefined : String(diagnostic.code);
                if (code && ignoredCodes.has(code)) {
                    return false;
                }
                return !patterns.some((pattern) => pattern.test(relative) || pattern.test(uri.fsPath));
            });
            if (config.get<boolean>('diagnosticLogging', false)) {
                log.appendLine(
                    `[diagnostics] ${relative}: ${filtered.length}/${diagnostics.length} published`,
                );
            }
            next(uri, filtered);
        },
    };
}

function showMissingServerActions(): void {
    if (missingServerWarningShown) {
        return;
    }
    missingServerWarningShown = true;
    const message =
        'pdx-ls was not found. Install it from the ParadoxCode release cache, select a binary, ' +
        'or add [server].binary to .pdx/project.toml.';
    log.appendLine(`WARNING: ${message}`);
    void vscode.window.showWarningMessage(
        `ParadoxCode: ${message}`,
        'Install pdx-ls',
        'Select binary',
        'Open Output',
    ).then((choice) => {
        if (choice === 'Install pdx-ls') {
            void vscode.commands.executeCommand('paradoxcode.installServer');
        } else if (choice === 'Select binary') {
            void vscode.commands.executeCommand('paradoxcode.selectServer');
        } else if (choice === 'Open Output') {
            log.show(true);
        }
    });
}

function createClient(): LanguageClient {
    const { command, source, missingOnPath } = resolveServerCommand();
    log.appendLine(`pdx-ls binary: ${command} (from ${source})`);
    if (missingOnPath) {
        showMissingServerActions();
    } else {
        missingServerWarningShown = false;
    }
    const serverOptions: ServerOptions = { command };
    const clientOptions: LanguageClientOptions = {
        // P0-4/P0-5 are intentionally not added here: localisation YAML and asset/sfx language
        // contributions remain outside this release's explicit VS Code scope.
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
        synchronize: {
            configurationSection: 'paradoxcode',
        },
        initializationOptions: readInitializationOptions(),
        revealOutputChannelOn: RevealOutputChannelOn.Error,
        middleware: diagnosticsMiddleware(),
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
            statusBar.tooltip = 'ParadoxCode: pdx-ls running (click to open output)';
            void vscode.commands.executeCommand('setContext', 'paradoxcodeServerRunning', true);
            break;
        case State.Starting:
            statusBar.text = 'PDX ◐';
            statusBar.tooltip = 'ParadoxCode: pdx-ls starting…';
            void vscode.commands.executeCommand('setContext', 'paradoxcodeServerRunning', false);
            break;
        default:
            statusBar.text = 'PDX ○';
            statusBar.tooltip = 'ParadoxCode: pdx-ls not running (click to open output)';
            void vscode.commands.executeCommand('setContext', 'paradoxcodeServerRunning', false);
    }
}

function startClient(loadedFiles?: LoadedFilesProvider): void {
    try {
        client = createClient();
        client.onDidChangeState((event) => {
            updateStatus(event.newState);
            if (event.newState === State.Running) {
                void loadedFiles?.refresh(client);
            }
        });
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

function stopClient(loadedFiles?: LoadedFilesProvider): void {
    if (client) {
        const previous = client;
        client = undefined;
        void previous.stop();
        updateStatus(State.Stopped);
        log.appendLine('language server client stopped');
    }
    loadedFiles?.clear();
}

async function chooseServerPath(): Promise<void> {
    const selected = await vscode.window.showOpenDialog({
        canSelectFiles: true,
        canSelectFolders: false,
        canSelectMany: false,
        openLabel: 'Use pdx-ls',
        filters: process.platform === 'win32' ? { Executable: ['exe', 'com', 'cmd', 'bat'] } : undefined,
    });
    if (!selected?.[0]) {
        return;
    }
    const target = vscode.workspace.workspaceFolders
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;
    await vscode.workspace.getConfiguration('paradoxcode').update(
        'pdxLsPath',
        selected[0].fsPath,
        target,
    );
    void vscode.window.showInformationMessage(`ParadoxCode: using ${selected[0].fsPath}`);
}

async function chooseGameDirectory(): Promise<void> {
    const selected = await vscode.window.showOpenDialog({
        canSelectFiles: false,
        canSelectFolders: true,
        canSelectMany: false,
        openLabel: 'Use EU4 installation',
    });
    if (!selected?.[0]) {
        return;
    }
    const target = vscode.workspace.workspaceFolders
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;
    await vscode.workspace.getConfiguration('paradoxcode').update(
        'gameDirectory',
        selected[0].fsPath,
        target,
    );
    void vscode.window.showInformationMessage(`ParadoxCode: using EU4 installation ${selected[0].fsPath}`);
}

async function exportDiagnostics(): Promise<void> {
    if (!client) {
        void vscode.window.showWarningMessage('ParadoxCode: the language server is not running.');
        return;
    }
    try {
        const report = await client.sendRequest<unknown>('pdx/workspaceDiagnostics', {
            offset: 0,
            limit: 128,
        });
        const target = await vscode.window.showSaveDialog({
            saveLabel: 'Export Diagnostics',
            filters: { JSON: ['json'] },
            defaultUri: vscode.Uri.file('paradoxcode-diagnostics.json'),
        });
        if (target) {
            await vscode.workspace.fs.writeFile(target, Buffer.from(JSON.stringify(report, null, 2), 'utf8'));
        }
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(`ParadoxCode: could not export diagnostics: ${message}`);
    }
}

async function installServer(context: vscode.ExtensionContext): Promise<void> {
    const config = vscode.workspace.getConfiguration('paradoxcode');
    const version = config.get<string>('serverVersion', DEFAULT_SERVER_VERSION);
    const repository = config.get<string>('serverRepository', DEFAULT_SERVER_REPOSITORY);
    const installDirectory = config.get<string>('serverInstallDirectory', '') || defaultInstallDirectory(context);
    try {
        const binary = await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Notification,
                title: 'ParadoxCode: installing pdx-ls',
                cancellable: false,
            },
            (progress) => installPdxLs(context, { version, repository, installDirectory }, progress),
        );
        const target = vscode.workspace.workspaceFolders
            ? vscode.ConfigurationTarget.Workspace
            : vscode.ConfigurationTarget.Global;
        await config.update('pdxLsPath', binary, target);
        void vscode.window.showInformationMessage(`ParadoxCode: pdx-ls ${version} installed.`);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR installing pdx-ls: ${message}`);
        const choice = await vscode.window.showErrorMessage(
            `ParadoxCode could not install pdx-ls: ${message}`,
            'Open Releases',
            'Open Output',
        );
        if (choice === 'Open Releases') {
            await vscode.env.openExternal(vscode.Uri.parse(`https://github.com/${repository}/releases`));
        } else if (choice === 'Open Output') {
            log.show(true);
        }
    }
}

let loadedFilesProvider: LoadedFilesProvider;

export function activate(context: vscode.ExtensionContext): void {
    extensionContext = context;
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? 'none';
    log.appendLine(`ParadoxCode extension activated (workspace root: ${root})`);
    context.subscriptions.push(log, statusBar);
    statusBar.show();

    loadedFilesProvider = new LoadedFilesProvider();
    context.subscriptions.push(
        loadedFilesProvider,
        vscode.window.registerTreeDataProvider('paradoxcode.loadedFiles', loadedFilesProvider),
    );

    startClient(loadedFilesProvider);

    const refresh = debounce(() => {
        void MissionPreviewPanel.refresh(client);
    }, 150);
    const restart = () => {
        stopClient(loadedFilesProvider);
        startClient(loadedFilesProvider);
        void MissionPreviewPanel.refresh(client);
    };

    context.subscriptions.push(
        vscode.commands.registerCommand('paradoxcode.showMissionPreview', () => {
            MissionPreviewPanel.show(context.extensionUri, client);
        }),
        vscode.commands.registerCommand('paradoxcode.openOutput', () => log.show(true)),
        vscode.commands.registerCommand('paradoxcode.selectServer', () => chooseServerPath()),
        vscode.commands.registerCommand('paradoxcode.installServer', () => installServer(context)),
        vscode.commands.registerCommand('paradoxcode.selectGameDirectory', () => chooseGameDirectory()),
        vscode.commands.registerCommand('paradoxcode.reloadServer', restart),
        vscode.commands.registerCommand('paradoxcode.exportDiagnostics', () => exportDiagnostics()),
        vscode.commands.registerCommand('paradoxcode.refreshLoadedFiles', () => loadedFilesProvider.refresh(client)),
        vscode.commands.registerCommand('paradoxcode.refreshMissionPreview', () => MissionPreviewPanel.refresh(client)),
        { dispose: () => MissionPreviewPanel.dispose() },
    );

    updateMissionContext(vscode.window.activeTextEditor?.document);
    context.subscriptions.push(
        vscode.window.onDidChangeActiveTextEditor((editor) => {
            updateMissionContext(editor?.document);
            refresh();
        }),
        vscode.workspace.onDidChangeTextDocument((event) => {
            if (isEu4Document(event.document)) {
                if (vscode.window.activeTextEditor?.document.uri.toString() === event.document.uri.toString()) {
                    updateMissionContext(event.document);
                }
                refresh();
            }
        }),
        vscode.workspace.onDidChangeWorkspaceFolders(() => restart()),
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (SERVER_SETTING_KEYS.some((key) => event.affectsConfiguration(`paradoxcode.${key}`))) {
                restart();
            } else if (event.affectsConfiguration('paradoxcode.preview')) {
                void MissionPreviewPanel.refresh(client);
            }
        }),
    );
}

export function deactivate(): void {
    stopClient(loadedFilesProvider);
    MissionPreviewPanel.dispose();
    statusBar.hide();
    extensionContext = undefined;
}
