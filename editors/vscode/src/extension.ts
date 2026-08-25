import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    LogMessageNotification,
    RevealOutputChannelOn,
    ServerOptions,
    State,
} from 'vscode-languageclient/node';

import { LoadedFilesProvider } from './fileExplorer';
import { MissionPreviewPanel } from './previewPanel';
import { readSharedConfig } from './sharedConfig';
import { findExecutableOnPath } from './serverPath';
import {
    DEFAULT_SERVER_REPOSITORY,
    cachedServerPath,
    defaultInstallDirectory,
    installPdxLs,
} from './serverInstaller';

const EU4_LANGUAGE_ID = 'eu4';
const LOCALISATION_LANGUAGE_ID = 'localisation';
const SERVER_SETTING_KEYS = [
    'pdxLsPath',
    'projectConfig',
    'modDirectory',
    'vanillaIndexCache',
    'dependencies',
    'gameDirectory',
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
let clientStartSequence = 0;
let serverReady = false;

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

function setVanillaContext(ready: boolean): void {
    void vscode.commands.executeCommand('setContext', 'paradoxcodeVanillaReady', ready);
}

function setServerReady(ready: boolean): void {
    serverReady = ready;
    void vscode.commands.executeCommand('setContext', 'paradoxcodeServerReady', ready);
}

function handleServerReady(
    readyClient: LanguageClient,
    loadedFiles?: LoadedFilesProvider,
): void {
    const wasReady = serverReady;
    setServerReady(true);
    updateStatus(readyClient.state);
    if (!wasReady && readyClient.state === State.Running) {
        void loadedFiles?.refresh(readyClient);
    }
}

/** Converts the server's user-facing Vanilla setup trail into a walkthrough state. */
function updateVanillaContext(message: string): void {
    if (/Vanilla symbols (?:are now enabled|loaded from)/i.test(message)
        || /Vanilla cache was regenerated .* loaded from/i.test(message)
        || /rebuilt from the discovered installation/i.test(message)) {
        setVanillaContext(true);
        return;
    }
    if (/continuing without Vanilla symbols/i.test(message)
        || /not a valid installation/i.test(message)
        || /(?:was|were) not found in common installation locations/i.test(message)
        || /multiple .* installations were found/i.test(message)
        || /discovery was skipped because it was already attempted/i.test(message)) {
        setVanillaContext(false);
    }
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

function installOptions(context: vscode.ExtensionContext) {
    const config = vscode.workspace.getConfiguration('paradoxcode');
    const packageVersion = context.extension.packageJSON.version;
    if (typeof packageVersion !== 'string' || !/^[0-9A-Za-z][0-9A-Za-z.+-]*$/.test(packageVersion)) {
        throw new Error('The ParadoxCode extension manifest has an invalid version.');
    }
    return {
        version: packageVersion,
        repository: DEFAULT_SERVER_REPOSITORY,
        installDirectory: config.get<string>('serverInstallDirectory', '') || defaultInstallDirectory(context),
    };
}

/** Resolves the pdx-ls binary. Explicit user/workspace configuration always wins over the
 * optional downloaded cache and PATH fallback. */
function resolveServerCommand(context: vscode.ExtensionContext): ServerResolution {
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
    const options = installOptions(context);
    const cached = cachedServerPath(context, options);
    if (cached) {
        return {
            command: cached,
            source: 'ParadoxCode checksum-verified server cache',
            missingOnPath: false,
        };
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

function showMissingServerActions(automaticInstallError?: string): void {
    if (missingServerWarningShown) {
        return;
    }
    missingServerWarningShown = true;
    const message = automaticInstallError
        ? `The automatic pdx-ls installation failed: ${automaticInstallError}`
        : 'pdx-ls was not found. Install it from the ParadoxCode release cache, select a binary, ' +
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

function createClient({ command, source }: ServerResolution): LanguageClient {
    log.appendLine(`pdx-ls binary: ${command} (from ${source})`);
    missingServerWarningShown = false;
    const serverOptions: ServerOptions = { command };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [
            { language: EU4_LANGUAGE_ID },
            { language: LOCALISATION_LANGUAGE_ID },
            { pattern: '**/common/achievements.txt' },
            { pattern: '**/common/alerts.txt' },
            { pattern: '**/common/graphicalculturetype.txt' },
            { pattern: '**/common/historial_lucky.txt' },
            { pattern: '**/common/technology.txt' },
            { pattern: '**/common/advisortypes/*.txt' },
            { pattern: '**/common/ages/*.txt' },
            { pattern: '**/common/ai_army/*.txt' },
            { pattern: '**/common/ai_attitudes/*.txt' },
            { pattern: '**/common/ai_personalities/*.txt' },
            { pattern: '**/common/ancestor_personalities/*.txt' },
            { pattern: '**/common/bookmarks/*.txt' },
            { pattern: '**/common/buildings/*.txt' },
            { pattern: '**/common/cb_types/*.txt' },
            { pattern: '**/common/centers_of_trade/*.txt' },
            { pattern: '**/common/church_aspects/*.txt' },
            { pattern: '**/common/client_states/*.txt' },
            { pattern: '**/common/colonial_regions/*.txt' },
            { pattern: '**/common/countries/*.txt' },
            { pattern: '**/common/country_colors/*.txt' },
            { pattern: '**/common/country_tags/*.txt' },
            { pattern: '**/common/cultures/*.txt' },
            { pattern: '**/common/custom_country_colors/*.txt' },
            { pattern: '**/common/custom_gui/*.txt' },
            { pattern: '**/common/custom_ideas/*.txt' },
            { pattern: '**/common/decrees/*.txt' },
            { pattern: '**/common/defender_of_faith/*.txt' },
            { pattern: '**/common/defines/*.txt' },
            { pattern: '**/common/diplomatic_actions/*.txt' },
            { pattern: '**/common/disasters/*.txt' },
            { pattern: '**/common/dynasty_colors/*.txt' },
            { pattern: '**/common/estate_agendas/*.txt' },
            { pattern: '**/common/estate_crown_land/*.txt' },
            { pattern: '**/common/estate_privileges/*.txt' },
            { pattern: '**/common/estates/*.txt' },
            { pattern: '**/common/estates_preload/*.txt' },
            { pattern: '**/common/event_modifiers/*.txt' },
            { pattern: '**/common/factions/*.txt' },
            { pattern: '**/common/federation_advancements/*.txt' },
            { pattern: '**/common/fervor/*.txt' },
            { pattern: '**/common/fetishist_cults/*.txt' },
            { pattern: '**/common/flagship_modifications/*.txt' },
            { pattern: '**/common/golden_bulls/*.txt' },
            { pattern: '**/common/government_mechanics/*.txt' },
            { pattern: '**/common/government_names/*.txt' },
            { pattern: '**/common/government_ranks/*.txt' },
            { pattern: '**/common/government_reforms/*.txt' },
            { pattern: '**/common/governments/*.txt' },
            { pattern: '**/common/great_projects/*.txt' },
            { pattern: '**/common/hegemons/*.txt' },
            { pattern: '**/common/holy_orders/*.txt' },
            { pattern: '**/common/ideas/*.txt' },
            { pattern: '**/common/imperial_incidents/*.txt' },
            { pattern: '**/common/imperial_reforms/*.txt' },
            { pattern: '**/common/incidents/*.txt' },
            { pattern: '**/common/institutions/*.txt' },
            { pattern: '**/common/insults/*.txt' },
            { pattern: '**/common/isolationism/*.txt' },
            { pattern: '**/common/leader_personalities/*.txt' },
            { pattern: '**/common/mercenary_companies/*.txt' },
            { pattern: '**/common/natives/*.txt' },
            { pattern: '**/common/naval_doctrines/*.txt' },
            { pattern: '**/common/new_diplomatic_actions/*.txt' },
            { pattern: '**/common/on_actions/*.txt' },
            { pattern: '**/common/opinion_modifiers/*.txt' },
            { pattern: '**/common/parliament_bribes/*.txt' },
            { pattern: '**/common/parliament_issues/*.txt' },
            { pattern: '**/common/peace_treaties/*.txt' },
            { pattern: '**/common/personal_deities/*.txt' },
            { pattern: '**/common/policies/*.txt' },
            { pattern: '**/common/powerprojection/*.txt' },
            { pattern: '**/common/prices/*.txt' },
            { pattern: '**/common/professionalism/*.txt' },
            { pattern: '**/common/province_names/*.txt' },
            { pattern: '**/common/province_triggered_modifiers/*.txt' },
            { pattern: '**/common/rebel_types/*.txt' },
            { pattern: '**/common/region_colors/*.txt' },
            { pattern: '**/common/religions/*.txt' },
            { pattern: '**/common/religious_conversions/*.txt' },
            { pattern: '**/common/religious_reforms/*.txt' },
            { pattern: '**/common/revolt_triggers/*.txt' },
            { pattern: '**/common/revolution/*.txt' },
            { pattern: '**/common/ruler_personalities/*.txt' },
            { pattern: '**/common/scripted_effects/*.txt' },
            { pattern: '**/common/scripted_functions/*.txt' },
            { pattern: '**/common/scripted_triggers/*.txt' },
            { pattern: '**/common/state_edicts/*.txt' },
            { pattern: '**/common/static_modifiers/*.txt' },
            { pattern: '**/common/subject_type_upgrades/*.txt' },
            { pattern: '**/common/subject_types/*.txt' },
            { pattern: '**/common/technologies/*.txt' },
            { pattern: '**/common/timed_modifiers/*.txt' },
            { pattern: '**/common/trade_companies/*.txt' },
            { pattern: '**/common/tradecompany_investments/*.txt' },
            { pattern: '**/common/tradegoods/*.txt' },
            { pattern: '**/common/tradenodes/*.txt' },
            { pattern: '**/common/trading_policies/*.txt' },
            { pattern: '**/common/triggered_modifiers/*.txt' },
            { pattern: '**/common/units/*.txt' },
            { pattern: '**/common/units_display/*.txt' },
            { pattern: '**/common/wargoal_types/*.txt' },
            { pattern: '**/customizable_localization/*.txt' },
            { pattern: '**/decisions/*.txt' },
            { pattern: '**/events/*.txt' },
            { pattern: '**/hints/*.txt' },
            { pattern: '**/history/advisors/*.txt' },
            { pattern: '**/history/countries/*.txt' },
            { pattern: '**/history/diplomacy/*.txt' },
            { pattern: '**/history/provinces/*.txt' },
            { pattern: '**/history/wars/*.txt' },
            { pattern: '**/map/ambient_object.txt' },
            { pattern: '**/map/area.txt' },
            { pattern: '**/map/climate.txt' },
            { pattern: '**/map/continent.txt' },
            { pattern: '**/map/lakes/00_lakes.txt' },
            { pattern: '**/map/positions.txt' },
            { pattern: '**/map/provincegroup.txt' },
            { pattern: '**/map/random/RNWScenarios.txt' },
            { pattern: '**/map/random/RandomLakeNames.txt' },
            { pattern: '**/map/random/RandomLandNames.txt' },
            { pattern: '**/map/random/RandomSeaNames.txt' },
            { pattern: '**/map/region.txt' },
            { pattern: '**/map/seasons.txt' },
            { pattern: '**/map/superregion.txt' },
            { pattern: '**/map/terrain.txt' },
            { pattern: '**/map/trade_winds.txt' },
            { pattern: '**/music/*.txt' },
            { pattern: '**/missions/*.txt' },
            { pattern: '**/sound/*.txt' },
            { pattern: '**/sound/amb/*.txt' },
            { pattern: '**/sound/battle/*.txt' },
            { pattern: '**/sound/battle/naval/*.txt' },
            { pattern: '**/tutorial/*.txt' },
            { pattern: '**/gfx/*.txt' },
            { pattern: '**/gfx/combat_result/*.txt' },
            { pattern: '**/gfx/sprite_packs/*.txt' },
            { pattern: '**/gfx/sprite_packs_order/*.txt' },
            { pattern: '**/interface/*.txt' },
            { pattern: '**/interface/*.gui' },
            { pattern: '**/interface/*.gfx' },
            { pattern: '**/interface/assets/*.gfx' },
            { pattern: '**/interface/government_mechanics/*.txt' },
            { pattern: '**/interface/government_mechanics/*.gui' },
            { pattern: '**/interface/government_mechanics/*.gfx' },
            { pattern: '**/interface/state_view/*.txt' },
            { pattern: '**/localisation/**/*' },
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
            statusBar.text = serverReady ? 'PDX ●' : 'PDX ◐';
            statusBar.tooltip = serverReady
                ? 'ParadoxCode: pdx-ls ready (click to open output)'
                : 'ParadoxCode: pdx-ls running; indexes are loading…';
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

async function resolveOrInstallServer(context: vscode.ExtensionContext): Promise<ServerResolution | undefined> {
    const resolution = resolveServerCommand(context);
    if (!resolution.missingOnPath) {
        return resolution;
    }

    // Marketplace installs should work without asking users to understand or install a language
    // server. Development/Test extension hosts keep the old explicit behavior so local tests do
    // not unexpectedly download release binaries.
    if (context.extensionMode !== vscode.ExtensionMode.Production) {
        showMissingServerActions();
        return undefined;
    }

    const options = installOptions(context);
    log.appendLine(`pdx-ls was not found; installing the matching ${options.version} release automatically`);
    statusBar.text = 'PDX ↓';
    statusBar.tooltip = 'ParadoxCode: installing the language server…';
    try {
        const binary = await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Window,
                title: 'ParadoxCode: preparing language support',
                cancellable: false,
            },
            (progress) => installPdxLs(context, options, progress),
        );
        log.appendLine(`pdx-ls ${options.version} installed and verified: ${binary}`);
        return {
            command: binary,
            source: 'automatic checksum-verified ParadoxCode installation',
            missingOnPath: false,
        };
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR installing pdx-ls automatically: ${message}`);
        updateStatus(State.Stopped);
        showMissingServerActions(message);
        return undefined;
    }
}

async function startClient(context: vscode.ExtensionContext, loadedFiles?: LoadedFilesProvider): Promise<void> {
    const sequence = ++clientStartSequence;
    setServerReady(false);
    setVanillaContext(false);
    try {
        const resolution = await resolveOrInstallServer(context);
        if (!resolution || sequence !== clientStartSequence) {
            return;
        }
        client = createClient(resolution);
        const currentClient = client;
        currentClient.onDidChangeState((event) => {
            if (client !== currentClient) {
                return;
            }
            updateStatus(event.newState);
            if (event.newState === State.Running && serverReady) {
                void loadedFiles?.refresh(currentClient);
            }
        });
        currentClient.onNotification(LogMessageNotification.type, (params) => {
            if (client !== currentClient) {
                return;
            }
            log.appendLine(`[pdx-ls] ${params.message}`);
            statusBar.tooltip = `ParadoxCode: ${params.message}`;
            updateVanillaContext(params.message);
        });
        currentClient.onNotification('pdx/ready', () => {
            if (client !== currentClient) {
                return;
            }
            handleServerReady(currentClient, loadedFiles);
        });
        updateStatus(currentClient.state);
        currentClient.start();
        log.appendLine('language server client started');
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR: ${message}`);
        void vscode.window.showErrorMessage(`ParadoxCode: ${message}`);
    }
}

async function stopClient(loadedFiles?: LoadedFilesProvider): Promise<void> {
    clientStartSequence += 1;
    setServerReady(false);
    if (client) {
        const previous = client;
        client = undefined;
        updateStatus(State.Stopped);
        log.appendLine('language server client stopped');
        try {
            await previous.stop();
        } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            log.appendLine(`WARNING stopping pdx-ls: ${message}`);
        }
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
        openLabel: 'Use EU4 installation for Vanilla data',
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
    setVanillaContext(false);
    void vscode.window.showInformationMessage(
        'ParadoxCode: EU4 installation selected. The language server will validate it and build Vanilla data in the background.',
    );
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

async function installServer(context: vscode.ExtensionContext): Promise<boolean> {
    const options = installOptions(context);
    try {
        const binary = await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Notification,
                title: 'ParadoxCode: installing pdx-ls',
                cancellable: false,
            },
            (progress) => installPdxLs(context, options, progress),
        );
        log.appendLine(`pdx-ls ${options.version} installed and verified: ${binary}`);
        void vscode.window.showInformationMessage(`ParadoxCode: pdx-ls ${options.version} is ready.`);
        return true;
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR installing pdx-ls: ${message}`);
        const choice = await vscode.window.showErrorMessage(
            `ParadoxCode could not install pdx-ls: ${message}`,
            'Open Releases',
            'Open Output',
        );
        if (choice === 'Open Releases') {
            await vscode.env.openExternal(vscode.Uri.parse(`https://github.com/${options.repository}/releases`));
        } else if (choice === 'Open Output') {
            log.show(true);
        }
        return false;
    }
}

let loadedFilesProvider: LoadedFilesProvider;

export function activate(context: vscode.ExtensionContext): void {
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? 'none';
    log.appendLine(`ParadoxCode extension activated (workspace root: ${root})`);
    setVanillaContext(false);
    context.subscriptions.push(log, statusBar);
    statusBar.show();

    loadedFilesProvider = new LoadedFilesProvider();
    context.subscriptions.push(
        loadedFilesProvider,
        vscode.window.registerTreeDataProvider('paradoxcode.loadedFiles', loadedFilesProvider),
    );

    const refresh = debounce(() => {
        void MissionPreviewPanel.refresh(client);
    }, 150);
    let restartTask = Promise.resolve();
    const restart = () => {
        restartTask = restartTask
            .catch((error: unknown) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`WARNING previous pdx-ls restart failed: ${message}`);
            })
            .then(async () => {
                await stopClient(loadedFilesProvider);
                await startClient(context, loadedFilesProvider);
                await MissionPreviewPanel.refresh(client);
            });
    };

    context.subscriptions.push(
        vscode.commands.registerCommand('paradoxcode.showMissionPreview', () => {
            MissionPreviewPanel.show(context.extensionUri, client);
        }),
        vscode.commands.registerCommand('paradoxcode.openOutput', () => log.show(true)),
        vscode.commands.registerCommand('paradoxcode.selectServer', () => chooseServerPath()),
        vscode.commands.registerCommand('paradoxcode.installServer', async () => {
            if (await installServer(context)) {
                restart();
            }
        }),
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

    void startClient(context, loadedFilesProvider);
}

export function deactivate(): Promise<void> {
    MissionPreviewPanel.dispose();
    statusBar.hide();
    return stopClient(loadedFilesProvider);
}
