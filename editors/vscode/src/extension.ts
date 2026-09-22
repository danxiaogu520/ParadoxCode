import * as vscode from 'vscode';
import * as path from 'path';
import {
    LanguageClient,
    LanguageClientOptions,
    LogMessageNotification,
    RevealOutputChannelOn,
    ServerOptions,
    State,
    Trace,
} from 'vscode-languageclient/node';

import { FileTeeDebugChannel } from './debugChannel';
import { registerAgentTools } from './agent/register';
import { setAgentClient } from './agent/server';
import { LoadedFilesProvider } from './fileExplorer';
import { MissionPreviewPanel } from './previewPanel';
import { MissionIconPickerPanel } from './iconPickerPanel';
import {
    PDCLOC_SCHEME,
    activateTransparentLocalisation,
    realUriOf,
} from './transparentLoc';
import {
    attachFollowupCompletionTrigger,
    attachSpritePreviewDocumentation,
    FOLLOWUP_COMPLETION_TRIGGER_COMMAND,
} from './completionMiddleware';
import { normalizeTexturePath, pngDataUrl } from './gameAssets';
import {
    appendTextureSection,
    extractSpriteHoverName,
    markdownFromHoverContents,
    texturefileHoverMarkdown,
    texturefileValueAt,
} from './hoverTextures';
import {
    cachedCardDataUrl,
    composeEventCard,
    composeMissionCard,
    eventCardMarkdown,
    missionCardMarkdown,
    parseHoverCardResponse,
} from './hoverCards';
import type {
    HoverCardAssetWire,
    HoverCardEventWire,
    HoverCardMissionWire,
    HoverCardWire,
} from './hoverCards';
import { findExecutableOnPath } from './serverPath';
import { globToRegExp, pathRelative } from './paths';
import {
    DEFAULT_SERVER_REPOSITORY,
    cachedServerPath,
    defaultInstallDirectory,
    installServerRelease,
} from './serverInstaller';

const EU4_LANGUAGE_ID = 'eu4';
const LOCALISATION_LANGUAGE_ID = 'localisation';
const SERVER_SETTING_KEYS = [
    'serverPath',
    'modDirectory',
    'vanillaIndexCache',
    'dependencies',
    'gameDirectory',
    'workspaceWideDiagnostics',
    'backgroundReindexIntervalMinutes',
    'backgroundReindexIdleSeconds',
    'ignoreFilePatterns',
    'ignoreDirectories',
    'diagnosticIgnoreCodes',
    'vanilla.mode',
    'diagnostics.severityOverrides',
    'localisation.preferredLanguages',
    'completion.sourceLayers',
    'performance.profile',
    'server.installPolicy',
    'serverInstallDirectory',
] as const;

interface DependencySetting {
    id: string;
    path: string;
    index?: string;
}

const DEPENDENCY_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;

/** Visible diagnostic trail: activation, binary resolution, server start. */
const log = vscode.window.createOutputChannel('ParadoxCode', { log: true });

/**
 * Debug trail: the server's INFO chatter, `pdc/trace` scheduling decisions, and
 * the verbose LSP protocol trace. Stays empty unless debug mode is on; the file
 * mirror behind `paradoxcode.debug.logFile` is part of the same wrapper.
 */
const debugLog = new FileTeeDebugChannel(
    vscode.window.createOutputChannel('ParadoxCode Debug', { log: true }),
    (message) => log.appendLine(message),
);

/** LSP MessageType: 1 = Error, 2 = Warning. Everything below is debug detail. */
const MESSAGE_TYPE_WARNING = 2;

/** Reads the live debug-mode switch; applies without a server restart. */
function debugModeEnabled(): boolean {
    return vscode.workspace.getConfiguration('paradoxcode')
        .get<boolean>('debug.enable', false);
}

/**
 * Resolves `paradoxcode.debug.logFile` to an absolute path. Relative paths are
 * anchored at the first workspace folder; a relative path without a workspace
 * is invalid and disables the mirror (with a one-line notice).
 */
function debugLogFilePath(): string | undefined {
    const configured = vscode.workspace.getConfiguration('paradoxcode')
        .get<string>('debug.logFile', '')
        .trim();
    if (!configured) {
        return undefined;
    }
    if (path.isAbsolute(configured)) {
        return configured;
    }
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!root) {
        log.appendLine(
            `WARNING: paradoxcode.debug.logFile "${configured}" is relative but no workspace folder is open; the debug log file mirror stays off.`,
        );
        return undefined;
    }
    return path.join(root, configured);
}

/** Last announced debug state; suppresses a startup log line when debug is off. */
let debugModeAnnounced: boolean | undefined;

/**
 * Applies the current debug settings live: the file-mirror target and the
 * protocol trace level of the running client. Never restarts the server —
 * debug mode exists to observe a live repro.
 */
function applyDebugConfiguration(): void {
    const enabled = debugModeEnabled();
    if (enabled !== debugModeAnnounced) {
        debugModeAnnounced = enabled;
        log.appendLine(`debug mode ${enabled ? 'enabled' : 'disabled'}`);
    }
    debugLog.updateTarget(enabled ? debugLogFilePath() : undefined);
    if (client) {
        client.setTrace(enabled ? Trace.Verbose : Trace.Off).catch(() => undefined);
        // vscode-languageclient never forwards $/setTrace itself, and the
        // server only sees the initialize-time `trace` parameter — so the
        // extension notifies it explicitly to gate the pdc/trace trail live.
        client
            .sendNotification('$/setTrace', { value: enabled ? 'verbose' : 'off' })
            .catch(() => undefined);
    }
}

/** Server state is deliberately compact so it works in narrow status bars. */
const statusBar = vscode.window.createStatusBarItem(
    'paradoxcode.status',
    vscode.StatusBarAlignment.Left,
    100,
);
statusBar.name = 'ParadoxCode Language Server';
statusBar.command = 'paradoxcode.openOutput';
statusBar.text = 'ParadoxCode $(debug-disconnect)';
statusBar.tooltip = vscode.l10n.t('ParadoxCode: server not running');

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
    if (!wasReady) {
        // The main channel deliberately shows only basic output; readiness is
        // the one milestone users wait for during the first index load.
        log.appendLine('pdc ready');
    }
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

/** Maps VS Code's `paradoxcode.*` settings onto server initialization options. */
function readInitializationOptions(): Record<string, unknown> {
    const config = vscode.workspace.getConfiguration('paradoxcode');
    const options: Record<string, unknown> = {};
    for (const key of [
        'modDirectory',
        'vanillaIndexCache',
        'dependencies',
        'gameDirectory',
    ] as const) {
        const value = config.get<unknown>(key);
        if (
            value !== undefined
            && value !== ''
            && (key !== 'dependencies' || isConfigurationExplicitlySet(config, key))
        ) {
            options[key] = value;
        }
    }
    const ignoredCodes = config.get<unknown>('diagnosticIgnoreCodes');
    if (
        Array.isArray(ignoredCodes)
        && (ignoredCodes.length > 0 || isConfigurationExplicitlySet(config, 'diagnosticIgnoreCodes'))
    ) {
        options.ignoredErrorCodes = ignoredCodes;
    }
    // Forwarded unconditionally: the setting's declared default (false) must reach the
    // server even when the user never touched it, otherwise the server-side default
    // (workspace-wide diagnostics on) would silently win.
    options.workspaceWideDiagnostics = config.get<boolean>('workspaceWideDiagnostics', false);
    const mappedSettings: ReadonlyArray<readonly [string, string]> = [
        ['backgroundReindexIntervalMinutes', 'backgroundReindexIntervalMinutes'],
        ['backgroundReindexIdleSeconds', 'backgroundReindexIdleSeconds'],
        ['ignoreFilePatterns', 'ignoreFilePatterns'],
        ['ignoreDirectories', 'ignoreDirectories'],
        ['vanilla.mode', 'vanillaMode'],
        ['diagnostics.severityOverrides', 'diagnosticSeverityOverrides'],
        ['localisation.preferredLanguages', 'preferredLocalisationLanguages'],
        ['completion.sourceLayers', 'completionSourceLayers'],
        ['performance.profile', 'performanceProfile'],
    ];
    for (const [setting, wireKey] of mappedSettings) {
        const value = config.get<unknown>(setting);
        const inspection = config.inspect<unknown>(setting);
        const explicitlyConfigured = inspection !== undefined && [
            inspection.globalValue,
            inspection.workspaceValue,
            inspection.workspaceFolderValue,
            inspection.globalLanguageValue,
            inspection.workspaceLanguageValue,
            inspection.workspaceFolderLanguageValue,
        ].some((candidate) => candidate !== undefined);
        if (explicitlyConfigured && value !== undefined) {
            options[wireKey] = value;
        }
    }
    return options;
}

function isConfigurationExplicitlySet(
    config: vscode.WorkspaceConfiguration,
    key: string,
): boolean {
    const inspection = config.inspect<unknown>(key);
    return inspection !== undefined && [
        inspection.globalValue,
        inspection.workspaceValue,
        inspection.workspaceFolderValue,
        inspection.globalLanguageValue,
        inspection.workspaceLanguageValue,
        inspection.workspaceFolderLanguageValue,
    ].some((candidate) => candidate !== undefined);
}

function dependencySettings(): DependencySetting[] {
    const raw = vscode.workspace
        .getConfiguration('paradoxcode')
        .get<unknown>('dependencies', []);
    if (!Array.isArray(raw)) {
        throw new Error(vscode.l10n.t('paradoxcode.dependencies must be an array'));
    }
    const dependencies: DependencySetting[] = [];
    for (const [index, value] of raw.entries()) {
        if (
            typeof value !== 'object'
            || value === null
            || typeof (value as { id?: unknown }).id !== 'string'
            || typeof (value as { path?: unknown }).path !== 'string'
        ) {
            throw new Error(vscode.l10n.t('paradoxcode.dependencies[{0}] must contain string id and path', index));
        }
        const id = (value as { id: string }).id.trim();
        const dependencyPath = (value as { path: string }).path.trim();
        const configuredIndex = (value as { index?: unknown }).index;
        if (!id || !dependencyPath) {
            throw new Error(vscode.l10n.t('paradoxcode.dependencies[{0}] must contain non-empty id and path', index));
        }
        if (!DEPENDENCY_ID_PATTERN.test(id)) {
            throw new Error(vscode.l10n.t('paradoxcode.dependencies[{0}] has invalid id "{1}"', index, id));
        }
        if (configuredIndex !== undefined && typeof configuredIndex !== 'string') {
            throw new Error(vscode.l10n.t('paradoxcode.dependencies[{0}].index must be a string', index));
        }
        const entry: DependencySetting = { id, path: dependencyPath };
        if (typeof configuredIndex === 'string' && configuredIndex.trim()) {
            entry.index = configuredIndex.trim();
        }
        dependencies.push(entry);
    }
    return dependencies;
}

function portableWorkspacePath(workspaceRoot: string, filePath: string): string {
    const relative = path.relative(workspaceRoot, filePath);
    if (!relative) {
        return '.';
    }
    // A different Windows drive cannot be represented as a relative path. Keep the absolute
    // path in that case; same-drive paths remain portable across machines with the workspace.
    return path.isAbsolute(relative) ? filePath : relative.split(path.sep).join('/');
}

function suggestedDependencyId(filePath: string): string {
    const suggested = path.basename(filePath)
        .replace(/[^A-Za-z0-9_.-]+/g, '-')
        .replace(/^[.-]+|[.-]+$/g, '');
    return suggested || 'dependency';
}

function workspaceConfigurationTarget(): vscode.WorkspaceFolder | undefined {
    return vscode.workspace.workspaceFolders?.[0];
}

async function openDependencySettings(): Promise<void> {
    await vscode.commands.executeCommand(
        'workbench.action.openSettings',
        '@id:paradoxcode.dependencies',
    );
}

// Cache refresh rides the restart: at initialize the server loads every persistent
// dependency/Vanilla cache and incrementally refreshes it against its source
// directory (fingerprint diff; only changed files are re-parsed). Cached roots are
// not file-watched mid-session, so this command is the manual pickup path.
function updateIndexCaches(restart: () => void): void {
    void vscode.window.showInformationMessage(
        vscode.l10n.t('ParadoxCode: refreshing persistent index caches — the language server is restarting…'),
    );
    restart();
}

async function addDependency(): Promise<void> {
    const workspaceFolder = workspaceConfigurationTarget();
    if (!workspaceFolder) {
        void vscode.window.showWarningMessage(
            vscode.l10n.t('ParadoxCode: open a workspace before adding a dependency.'),
        );
        return;
    }
    const selected = await vscode.window.showOpenDialog({
        canSelectFiles: false,
        canSelectFolders: true,
        canSelectMany: false,
        openLabel: vscode.l10n.t('Use Dependency Mod'),
        title: vscode.l10n.t('Choose a dependency Mod directory'),
    });
    if (!selected?.[0]) {
        return;
    }
    try {
        const stat = await vscode.workspace.fs.stat(selected[0]);
        if ((stat.type & vscode.FileType.Directory) === 0) {
            void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: the dependency path must be a directory.'));
            return;
        }
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: could not read dependency directory: {0}', message));
        return;
    }

    let dependencies: DependencySetting[];
    try {
        dependencies = dependencySettings();
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: {0}', message));
        return;
    }
    const id = await vscode.window.showInputBox({
        title: vscode.l10n.t('Name the dependency'),
        prompt: vscode.l10n.t('Use a stable id; dependencies are ordered from lowest to highest priority.'),
        value: suggestedDependencyId(selected[0].fsPath),
        validateInput: (value) => {
            const normalized = value.trim();
            if (!normalized) {
                return vscode.l10n.t('Dependency id must not be empty.');
            }
            if (!DEPENDENCY_ID_PATTERN.test(normalized)) {
                return vscode.l10n.t('Use letters, numbers, dots, hyphens, or underscores.');
            }
            if (dependencies.some((dependency) => dependency.id.toLowerCase() === normalized.toLowerCase())) {
                return vscode.l10n.t('A dependency named {0} already exists.', normalized);
            }
            return undefined;
        },
    });
    if (!id) {
        return;
    }

    const cacheChoice = await vscode.window.showQuickPick([
        {
            label: vscode.l10n.t('Persistent index cache (recommended)'),
            description: vscode.l10n.t('Loads a .pdcindex and re-checks only the files that changed.'),
            detail: vscode.l10n.t('Pros: fast startup even for large mods. Cons: changes made while the server runs are picked up only after Update Index Caches.'),
            value: 'index',
        },
        {
            label: vscode.l10n.t('Live scan'),
            description: vscode.l10n.t('Scans the directory on every start and keeps watching it.'),
            detail: vscode.l10n.t('Pros: dependency changes are picked up automatically. Cons: large mods slow down startup and can cause lag.'),
            value: 'live',
        },
    ], {
        title: vscode.l10n.t('How should ParadoxCode load this dependency?'),
        placeHolder: vscode.l10n.t('Choose a loading strategy'),
    });
    if (!cacheChoice) {
        return;
    }

    const workspaceRoot = workspaceFolder.uri.fsPath;
    const entry: DependencySetting = {
        id: id.trim(),
        path: portableWorkspacePath(workspaceRoot, selected[0].fsPath),
    };
    if (cacheChoice.value === 'index') {
        const defaultIndex = vscode.Uri.file(
            path.join(workspaceRoot, '.pdc', 'indexes', `${entry.id}.pdcindex`),
        );
        const index = await vscode.window.showSaveDialog({
            defaultUri: defaultIndex,
            filters: { [vscode.l10n.t('ParadoxCode index')]: ['pdcindex'] },
            saveLabel: vscode.l10n.t('Use Index Path'),
            title: vscode.l10n.t('Choose the dependency index cache path'),
        });
        if (!index) {
            return;
        }
        entry.index = portableWorkspacePath(workspaceRoot, index.fsPath);
    }

    dependencies.push(entry);
    try {
        await vscode.workspace.getConfiguration('paradoxcode').update(
            'dependencies',
            dependencies,
            vscode.ConfigurationTarget.Workspace,
        );
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: could not save dependency: {0}', message));
        return;
    }
    const action = await vscode.window.showInformationMessage(
        vscode.l10n.t('ParadoxCode: added dependency {0} at the end of the priority list.', entry.id),
        vscode.l10n.t('Open Dependencies Settings'),
    );
    if (action) {
        await openDependencySettings();
    }
}

async function removeDependency(): Promise<void> {
    if (!workspaceConfigurationTarget()) {
        void vscode.window.showWarningMessage(
            vscode.l10n.t('ParadoxCode: open a workspace before removing a dependency.'),
        );
        return;
    }
    let dependencies: DependencySetting[];
    try {
        dependencies = dependencySettings();
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: {0}', message));
        return;
    }
    if (dependencies.length === 0) {
        void vscode.window.showInformationMessage(vscode.l10n.t('ParadoxCode: no workspace dependencies are configured.'));
        return;
    }
    const selected = await vscode.window.showQuickPick(
        dependencies.map((dependency, index) => ({
            label: dependency.id,
            description: dependency.path,
            detail: dependency.index
                ? vscode.l10n.t('index: {0}', dependency.index)
                : vscode.l10n.t('live scan'),
            index,
        })),
        {
            title: vscode.l10n.t('Remove a ParadoxCode dependency'),
            placeHolder: vscode.l10n.t('Choose a dependency'),
        },
    );
    if (!selected) {
        return;
    }
    const removeButton = vscode.l10n.t('Remove');
    const confirmation = await vscode.window.showWarningMessage(
        vscode.l10n.t('Remove dependency {0}?', selected.label),
        { modal: true },
        removeButton,
    );
    if (confirmation !== removeButton) {
        return;
    }
    dependencies.splice(selected.index, 1);
    try {
        await vscode.workspace.getConfiguration('paradoxcode').update(
            'dependencies',
            dependencies,
            vscode.ConfigurationTarget.Workspace,
        );
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: could not save dependency: {0}', message));
        return;
    }
    void vscode.window.showInformationMessage(vscode.l10n.t('ParadoxCode: removed dependency {0}.', selected.label));
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
        throw new Error(vscode.l10n.t('The ParadoxCode extension manifest has an invalid version.'));
    }
    return {
        version: packageVersion,
        repository: DEFAULT_SERVER_REPOSITORY,
        installDirectory: config.get<string>('serverInstallDirectory', '') || defaultInstallDirectory(context),
    };
}

/** Resolves the ParadoxCode server binary. Explicit user/workspace configuration always wins over the
 * optional downloaded cache and PATH fallback. */
function resolveServerCommand(context: vscode.ExtensionContext): ServerResolution {
    const configuration = vscode.workspace.getConfiguration('paradoxcode');
    const configuredPath = configuration.get<string>('serverPath', '');
    if (configuredPath) {
        return {
            command: configuredPath,
            source: 'setting paradoxcode.serverPath',
            missingOnPath: false,
        };
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
        command: 'paradoxcode',
        source: '$PATH (paradoxcode)',
        missingOnPath: findExecutableOnPath('paradoxcode') === undefined,
    };
}

function diagnosticIgnorePatterns(): string[] {
    return vscode.workspace
        .getConfiguration('paradoxcode')
        .get<string[]>('diagnosticIgnoreFiles', [])
        .filter((value): value is string => typeof value === 'string' && value.length > 0);
}

/** Workspace-relative diagnostic path (forward slashes; full path when
 * outside every folder). `pdcloc://` decoded views mirror their backing
 * `file://` path but `getWorkspaceFolder` only matches `file` URIs, so the
 * twin is unwrapped first and ignore patterns keep matching the logical
 * path. Exported for the extension-host contract test. */
export function relativeDiagnosticPath(uri: vscode.Uri): string {
    const real = realUriOf(uri) ?? uri;
    const folder = vscode.workspace.getWorkspaceFolder(real);
    if (!folder) {
        return real.fsPath.replace(/\\/g, '/');
    }
    return pathRelative(folder.uri.fsPath, real.fsPath);
}

function previewRefreshMode(): 'always' | 'onSave' | 'manual' {
    const value = vscode.workspace
        .getConfiguration('paradoxcode.preview')
        .get<string>('refreshMode', 'always');
    return value === 'onSave' || value === 'manual' ? value : 'always';
}

/** Reads the completion sprite-preview switch (on unless opted out). */
function completionSpritePreview(): boolean {
    return vscode.workspace
        .getConfiguration('paradoxcode.completion')
        .get<boolean>('iconPreview', true);
}

function clientMiddleware(): NonNullable<LanguageClientOptions['middleware']> {
    return {
        provideCompletionItem(document, position, context, token, next) {
            return Promise.resolve(next(document, position, context, token)).then((result) => {
                if (!result) {
                    return result;
                }
                const items = Array.isArray(result) ? result : result.items;
                for (const item of items) {
                    attachFollowupCompletionTrigger(item, document.uri.toString());
                }
                return result;
            });
        },
        resolveCompletionItem(item, token, next) {
            return Promise.resolve(next(item, token)).then(async (resolved) => {
                if (resolved) {
                    // VS Code may resolve an item before applying it. Re-attach the command to
                    // the resolved object because the server's resolve response is authoritative.
                    attachFollowupCompletionTrigger(resolved);
                    if (completionSpritePreview()) {
                        // A resolved label naming a sprite gains the decoded
                        // first-frame image in its documentation; resolve fires
                        // per displayed item, so nothing is decoded up front.
                        // Awaited, because VS Code snapshots the item when this
                        // promise settles — a later mutation would be lost.
                        await attachSpritePreviewDocumentation(resolved, MissionPreviewPanel.store())
                            .catch(() => undefined);
                    }
                }
                return resolved;
            });
        },
        provideHover(document, position, token, next) {
            return Promise.resolve(next(document, position, token)).then((hover) =>
                augmentHoverTexturePreview(document, position, hover, token),
            );
        },
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

/** Returns the markdown string of a hover whose contents the language
 * server produced as MarkupContent markdown (pdc's only hover shape). */
function hoverMarkdownString(hover: vscode.Hover): vscode.MarkdownString | undefined {
    return markdownFromHoverContents(hover.contents) as vscode.MarkdownString | undefined;
}

/** Timeout for `pdc/hoverCard`: hover must answer, never hang. */
const HOVER_CARD_TIMEOUT_MS = 1500;

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
    return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`request timed out after ${ms}ms`)), ms);
        promise.then(
            (value) => {
                clearTimeout(timer);
                resolve(value);
            },
            (error) => {
                clearTimeout(timer);
                reject(error);
            },
        );
    });
}

/**
 * Asks the server for a structured hover card (`pdc/hoverCard`): the
 * mission/sprite/texture at this position with server-resolved texture
 * paths. Any failure — old server without the method, cancellation,
 * timeout, malformed payload — yields `undefined` so the caller falls
 * back to the legacy hover paths.
 */
async function requestHoverCard(
    document: vscode.TextDocument,
    position: vscode.Position,
    token: vscode.CancellationToken,
): Promise<HoverCardWire | undefined> {
    const server = client;
    if (!server) {
        return undefined;
    }
    try {
        const payload = await withTimeout(
            server.sendRequest('pdc/hoverCard', {
                textDocument: { uri: document.uri.toString() },
                position: { line: position.line, character: position.character },
            }, token),
            HOVER_CARD_TIMEOUT_MS,
        );
        return parseHoverCardResponse(payload)?.card;
    } catch {
        return undefined;
    }
}

/** Appends a markdown section to the server hover (standalone when absent). */
function combineHoverMarkdown(
    hover: vscode.Hover | null | undefined,
    section: string,
): vscode.Hover | null {
    const markdown = hover ? hoverMarkdownString(hover) : undefined;
    if (hover && markdown) {
        const augmented = new vscode.MarkdownString(`${markdown.value}\n\n${section}`);
        augmented.isTrusted = true;
        return new vscode.Hover(augmented, hover.range);
    }
    if (hover) {
        // A non-markdown server hover keeps its own shape untouched.
        return hover;
    }
    const standalone = new vscode.MarkdownString(section);
    standalone.isTrusted = true;
    return new vscode.Hover(standalone);
}

/**
 * Renders the server's structured hover card: mission positions get the
 * composed game-look mission card, event positions the composed event
 * window, sprite and texture positions a single-texture preview. Returns
 * `undefined` when this card cannot be rendered, letting the caller fall
 * back to the legacy hover paths.
 */
async function augmentHoverWithCard(
    hover: vscode.Hover | null | undefined,
    card: HoverCardWire,
): Promise<vscode.Hover | null | undefined> {
    if (card.kind === 'mission') {
        if (!card.mission) {
            return undefined;
        }
        const section = await missionCardSection(card.mission, card.asset, card.cardAssets ?? {});
        // A mission card that could not compose degrades to the plain
        // server hover — the legacy paths have nothing for mission spans.
        return section ? combineHoverMarkdown(hover, section) : hover ?? null;
    }
    if (card.kind === 'event') {
        if (!card.event) {
            return undefined;
        }
        const section = await eventCardSection(card.event, card.asset, card.cardAssets ?? {});
        // Same degradation contract as the mission card.
        return section ? combineHoverMarkdown(hover, section) : hover ?? null;
    }
    const asset = card.asset;
    if (!asset) {
        return undefined;
    }
    const store = MissionPreviewPanel.store();
    const image = await store.textureFile(asset.path);
    if (!image) {
        return undefined;
    }
    const section = texturefileHoverMarkdown({
        name: asset.sprite ?? path.basename(asset.path),
        rel: asset.path,
        url: image.url,
        width: image.width,
    });
    return combineHoverMarkdown(hover, section);
}

/**
 * Composes the game-look mission card — frame, icon underneath,
 * §-coloured title — and returns its markdown image section. `undefined`
 * when no decodable assets are left.
 */
async function missionCardSection(
    mission: HoverCardMissionWire,
    asset: HoverCardAssetWire | undefined,
    cardAssets: NonNullable<HoverCardWire['cardAssets']>,
): Promise<string | undefined> {
    const store = MissionPreviewPanel.store();
    const load = (candidate: HoverCardAssetWire | undefined) =>
        candidate ? store.textureRaster(candidate.path) : Promise.resolve(undefined);
    const [frame, icon, fonts] = await Promise.all([
        load(cardAssets.frame),
        load(asset),
        store.loadFontRasters(),
    ]);
    if (!frame && !icon) {
        return undefined;
    }
    // Recompose only when the mission or any input asset changed: the key
    // folds the mission identity and every asset path plus its mtime.
    const stampAssets = [cardAssets.frame, asset];
    const cacheKey = [
        mission.id,
        mission.title?.value ?? '',
        ...stampAssets.map((candidate) => (candidate ? `${candidate.path}@${store.mtimeOf(candidate.path) ?? '?'}` : '-')),
    ].join('\0');
    const dataUrl = cachedCardDataUrl(cacheKey, () =>
        pngDataUrl(composeMissionCard(mission, {
            frame,
            icon,
            iconFrames: asset?.frames,
            fonts,
        })),
    );
    return missionCardMarkdown(dataUrl);
}

/**
 * Composes the game-look event window — stacked background chrome, picture
 * banner, §-coloured title/description, one button row per option — and
 * returns its markdown image section. `undefined` when no decodable assets
 * are left.
 */
async function eventCardSection(
    event: HoverCardEventWire,
    asset: HoverCardAssetWire | undefined,
    cardAssets: NonNullable<HoverCardWire['cardAssets']>,
): Promise<string | undefined> {
    const store = MissionPreviewPanel.store();
    const load = (candidate: HoverCardAssetWire | undefined) =>
        candidate ? store.textureRaster(candidate.path) : Promise.resolve(undefined);
    const [backgroundTop, backgroundMiddle, bottomS, bottomM, bottomL, optionButton, picture, fonts] =
        await Promise.all([
            load(cardAssets.backgroundTop),
            load(cardAssets.backgroundMiddle),
            load(cardAssets.backgroundBottomS),
            load(cardAssets.backgroundBottomM),
            load(cardAssets.backgroundBottomL),
            load(cardAssets.optionButton),
            load(asset),
            store.loadFontRasters(),
        ]);
    if (!backgroundTop && !picture) {
        return undefined;
    }
    // Recompose only when the event text or any input asset changed: the
    // key folds the text payload and every asset path plus its mtime.
    const stampAssets = [
        cardAssets.backgroundTop,
        cardAssets.backgroundMiddle,
        cardAssets.backgroundBottomS,
        cardAssets.backgroundBottomM,
        cardAssets.backgroundBottomL,
        cardAssets.optionButton,
        asset,
    ];
    const cacheKey = [
        event.id,
        event.title?.value ?? '',
        event.desc?.value ?? '',
        ...event.options.map((option) => option.name?.value ?? option.nameKey),
        ...stampAssets.map((candidate) => (candidate ? `${candidate.path}@${store.mtimeOf(candidate.path) ?? '?'}` : '-')),
    ].join('\0');
    const dataUrl = cachedCardDataUrl(cacheKey, () =>
        pngDataUrl(composeEventCard(event, {
            backgroundTop,
            backgroundMiddle,
            backgroundBottomS: bottomS,
            backgroundBottomM: bottomM,
            backgroundBottomL: bottomL,
            optionButton,
            picture,
            fonts,
        })),
    );
    return eventCardMarkdown(dataUrl);
}

/**
 * Appends decoded texture previews to hovers. The server's structured
 * hover card (mission/sprite/texture) is preferred; when the protocol is
 * unavailable the legacy paths take over: sprite symbol hovers get a
 * `#### Texture` section and a hovered `texturefile` value in a `.gfx`
 * file gets the preview appended to the server's semantic hover or as a
 * standalone section. Every failure degrades to the untouched server
 * hover — the preview must never fail.
 */
async function augmentHoverTexturePreview(
    document: vscode.TextDocument,
    position: vscode.Position,
    hover: vscode.Hover | null | undefined,
    token: vscode.CancellationToken,
): Promise<vscode.Hover | null | undefined> {
    const hoverConfig = vscode.workspace.getConfiguration('paradoxcode.hover');
    const texturePreview = hoverConfig.get<boolean>('texturePreview', true);
    const missionCard = hoverConfig.get<boolean>('missionCard', true);
    const eventCard = hoverConfig.get<boolean>('eventCard', true);
    if (texturePreview || missionCard || eventCard) {
        const card = await requestHoverCard(document, position, token);
        const kindEnabled = card?.kind === 'mission'
            ? missionCard
            : card?.kind === 'event'
                ? eventCard
                : texturePreview;
        if (card && kindEnabled) {
            const handled = await augmentHoverWithCard(hover, card);
            if (handled !== undefined) {
                return handled;
            }
        }
    }
    if (!texturePreview) {
        return hover;
    }
    if (document.uri.fsPath.toLowerCase().endsWith('.gfx')) {
        const value = texturefileValueAt(document.lineAt(position.line).text, position.character);
        if (value !== undefined) {
            const rel = normalizeTexturePath(value);
            const store = MissionPreviewPanel.store();
            const file = rel === '' ? undefined : store.resolveTexture(rel);
            const image = file ? await store.textureFile(file) : undefined;
            if (file && image) {
                const section = texturefileHoverMarkdown({
                    name: path.basename(rel),
                    rel,
                    url: image.url,
                    width: image.width,
                });
                const markdown = hover ? hoverMarkdownString(hover) : undefined;
                if (hover && markdown) {
                    const augmented = new vscode.MarkdownString(`${markdown.value}\n\n${section}`);
                    augmented.isTrusted = true;
                    return new vscode.Hover(augmented, hover.range);
                }
                if (hover) {
                    // A non-markdown server hover keeps its own shape untouched.
                    return hover;
                }
                const standalone = new vscode.MarkdownString(section);
                standalone.isTrusted = true;
                return new vscode.Hover(standalone);
            }
            // No decodable preview (missing or unhandled format): the server
            // hover already reports resolution, so pass it through.
            return hover ?? null;
        }
    }
    if (hover) {
        const markdown = hoverMarkdownString(hover);
        const name = markdown ? extractSpriteHoverName(markdown.value) : undefined;
        if (!markdown || !name) {
            return hover;
        }
        // Only sprite hovers pay for the asset store (its generation key
        // probes the workshop font-mod directory); everything else returns
        // the server hover untouched.
        const store = MissionPreviewPanel.store();
        const entry = store.spriteTexture(name);
        const file = entry ? store.resolveTexture(entry.textureFile) : undefined;
        const image = file ? await store.textureFile(file) : undefined;
        if (!entry || !image) {
            return hover;
        }
        const augmented = new vscode.MarkdownString(
            appendTextureSection(markdown.value, {
                name,
                rel: entry.textureFile,
                frames: entry.frames,
                url: image.url,
                width: image.width,
            }),
        );
        augmented.isTrusted = true;
        return new vscode.Hover(augmented, hover.range);
    }
    return hover ?? null;
}

function showMissingServerActions(automaticInstallError?: string): void {
    if (missingServerWarningShown) {
        return;
    }
    missingServerWarningShown = true;
    const message = automaticInstallError
        ? vscode.l10n.t('The automatic ParadoxCode server installation failed: {0}', automaticInstallError)
        : vscode.l10n.t(
            'The ParadoxCode server was not found. Install it from the release cache, select a '
            + 'binary, set paradoxcode.serverPath, or add paradoxcode to PATH.',
        );
    log.appendLine(`WARNING: ${message}`);
    const installButton = vscode.l10n.t('Install server');
    const selectBinaryButton = vscode.l10n.t('Select binary');
    const openOutputButton = vscode.l10n.t('Open Output');
    void vscode.window.showWarningMessage(
        vscode.l10n.t('ParadoxCode: {0}', message),
        installButton,
        selectBinaryButton,
        openOutputButton,
    ).then((choice) => {
        if (choice === installButton) {
            void vscode.commands.executeCommand('paradoxcode.installServer');
        } else if (choice === selectBinaryButton) {
            void vscode.commands.executeCommand('paradoxcode.selectServer');
        } else if (choice === openOutputButton) {
            log.show(true);
        }
    });
}

function createClient({ command, source }: ServerResolution): LanguageClient {
    log.appendLine(`ParadoxCode server binary: ${command} (from ${source})`);
    missingServerWarningShown = false;
    const serverOptions: ServerOptions = { command };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [
            { language: EU4_LANGUAGE_ID },
            { language: LOCALISATION_LANGUAGE_ID },
            // Decoded views over transcoded files: the provider syncs decoded
            // text under the pdcloc:// scheme; the server resolves these URIs
            // to the backing path (see crates/pdc/src/uri.rs).
            { scheme: PDCLOC_SCHEME, language: EU4_LANGUAGE_ID },
            { scheme: PDCLOC_SCHEME, language: LOCALISATION_LANGUAGE_ID },
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
        // Deliberately no `synchronize.configurationSection`: its auto-push wraps the
        // settings in a `paradoxcode` namespace the server's flat-key configuration
        // handler never reads, so the push never applied anything. Every setting the
        // server consumes lives in SERVER_SETTING_KEYS, whose save path restarts the
        // server with fresh initializationOptions; the push only raced that restart
        // and surfaced as a spurious didChangeConfiguration send failure.
        initializationOptions: readInitializationOptions(),
        // The client writes server stderr and its own diagnostics into the main
        // channel so users have exactly one basic-output channel to watch.
        outputChannel: log,
        traceOutputChannel: debugLog,
        revealOutputChannelOn: RevealOutputChannelOn.Error,
        middleware: clientMiddleware(),
    };
    const languageClient = new LanguageClient(
        'paradoxcode',
        'ParadoxCode Language Server',
        serverOptions,
        clientOptions,
    );
    if (debugModeEnabled()) {
        // Setting the trace before start makes the initialize request itself
        // carry `trace: "verbose"` and attaches the protocol tracer from the
        // first frame; a later enable goes through `applyDebugConfiguration`.
        languageClient.setTrace(Trace.Verbose).catch(() => undefined);
    }
    return languageClient;
}

function updateStatus(state: State): void {
    switch (state) {
        case State.Running:
            statusBar.text = serverReady
                ? 'ParadoxCode $(check)'
                : 'ParadoxCode $(sync~spin)';
            statusBar.tooltip = serverReady
                ? vscode.l10n.t('ParadoxCode: pdc ready (click to open output)')
                : vscode.l10n.t('ParadoxCode: pdc running; indexes are loading…');
            void vscode.commands.executeCommand('setContext', 'paradoxcodeServerRunning', true);
            break;
        case State.Starting:
            statusBar.text = 'ParadoxCode $(sync~spin)';
            statusBar.tooltip = vscode.l10n.t('ParadoxCode: pdc starting…');
            void vscode.commands.executeCommand('setContext', 'paradoxcodeServerRunning', false);
            break;
        default:
            statusBar.text = 'ParadoxCode $(debug-disconnect)';
            statusBar.tooltip = vscode.l10n.t('ParadoxCode: pdc not running (click to open output)');
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

    const installPolicy = vscode.workspace
        .getConfiguration('paradoxcode')
        .get<'auto' | 'prompt' | 'never'>('server.installPolicy', 'auto');
    if (installPolicy !== 'auto') {
        showMissingServerActions();
        return undefined;
    }

    const options = installOptions(context);
    log.appendLine(`pdc was not found; installing the matching ${options.version} release automatically`);
    statusBar.text = 'ParadoxCode $(cloud-download~spin)';
    statusBar.tooltip = vscode.l10n.t('ParadoxCode: installing the language server…');
    try {
        const binary = await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Window,
                title: vscode.l10n.t('ParadoxCode: preparing language support'),
                cancellable: false,
            },
            (progress) => installServerRelease(context, options, progress),
        );
        log.appendLine(`pdc ${options.version} installed and verified: ${binary}`);
        return {
            command: binary,
            source: 'automatic checksum-verified ParadoxCode installation',
            missingOnPath: false,
        };
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR installing pdc automatically: ${message}`);
        updateStatus(State.Stopped);
        showMissingServerActions(message);
        return undefined;
    }
}

function completionDocumentUri(argument: unknown): string | undefined {
    if (!argument || typeof argument !== 'object' || !('uri' in argument)) {
        return undefined;
    }
    const uri = (argument as { uri?: unknown }).uri;
    return typeof uri === 'string' ? uri : undefined;
}

/**
 * Requests a follow-up list after an assignment or block inserted by a completion has reached the
 * editor document. The timer yields once so the language client's didChange notification is sent
 * before the follow-up completion request observes the new text.
 */
function triggerFollowupCompletion(argument?: unknown): void {
    const expectedUri = completionDocumentUri(argument);
    setTimeout(() => {
        const editor = vscode.window.activeTextEditor;
        if (!editor || (expectedUri && editor.document.uri.toString() !== expectedUri)) {
            return;
        }
        void vscode.commands.executeCommand('editor.action.triggerSuggest');
    }, 0);
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
        // Publish the fresh instance for the agent tool layer before start() resolves:
        // tools poll for a Running client, so early calls simply wait.
        setAgentClient(currentClient);
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
            // Errors and warnings are basic output; the INFO/LOG trail (index
            // phases, quiet scans, worker reports) belongs to the debug
            // channel. Parsing and the status-bar mirror must keep running
            // for every message: the Vanilla walkthrough state machine reacts
            // to INFO message text.
            if (params.type <= MESSAGE_TYPE_WARNING) {
                log.appendLine(`[pdc] ${params.message}`);
            } else if (debugModeEnabled()) {
                // Leveled (not appendLine) so every debug line carries a
                // timestamp, aligning with the timestamped protocol trace.
                debugLog.info(`[pdc] ${params.message}`);
            }
            statusBar.tooltip = vscode.l10n.t('ParadoxCode: {0}', params.message);
            updateVanillaContext(params.message);
        });
        currentClient.onNotification('pdc/trace', (params: unknown) => {
            if (client !== currentClient) {
                return;
            }
            const message = (params as { message?: unknown } | undefined)?.message;
            if (debugModeEnabled() && typeof message === 'string') {
                debugLog.info(`[trace] ${message}`);
            }
        });
        currentClient.onNotification('pdc/ready', () => {
            if (client !== currentClient) {
                return;
            }
            handleServerReady(currentClient, loadedFiles);
        });
        updateStatus(currentClient.state);
        currentClient.start();
        log.appendLine('language server client started');
        debugLog.markSession(`binary ${resolution.command}`);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR: ${message}`);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: {0}', message));
    }
}

async function stopClient(loadedFiles?: LoadedFilesProvider): Promise<void> {
    clientStartSequence += 1;
    setServerReady(false);
    if (client) {
        const previous = client;
        client = undefined;
        setAgentClient(undefined);
        updateStatus(State.Stopped);
        log.appendLine('language server client stopped');
        try {
            await previous.stop();
        } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            log.appendLine(`WARNING stopping pdc: ${message}`);
        }
    }
    loadedFiles?.clear();
}

/**
 * Flips `paradoxcode.debug.enable` in user- or workspace scope. The resulting
 * configuration change drives `applyDebugConfiguration`; no restart happens.
 */
async function toggleDebugMode(): Promise<void> {
    const config = vscode.workspace.getConfiguration('paradoxcode');
    const enabled = config.get<boolean>('debug.enable', false);
    const target = vscode.workspace.workspaceFolders
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;
    await config.update('debug.enable', !enabled, target);
    if (!enabled) {
        const openDebugButton = vscode.l10n.t('Open Debug Output');
        void vscode.window.showInformationMessage(
            vscode.l10n.t(
                'ParadoxCode: debug mode enabled. Reproduce the issue, then share the "ParadoxCode Debug" output.',
            ),
            openDebugButton,
        ).then((choice) => {
            if (choice === openDebugButton) {
                debugLog.show(true);
            }
        });
    }
}

async function chooseServerPath(): Promise<void> {
    const selected = await vscode.window.showOpenDialog({
        canSelectFiles: true,
        canSelectFolders: false,
        canSelectMany: false,
        openLabel: vscode.l10n.t('Use pdc'),
        filters: process.platform === 'win32'
            ? { [vscode.l10n.t('Executable')]: ['exe', 'com', 'cmd', 'bat'] }
            : undefined,
    });
    if (!selected?.[0]) {
        return;
    }
    const target = vscode.workspace.workspaceFolders
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;
    await vscode.workspace.getConfiguration('paradoxcode').update(
        'serverPath',
        selected[0].fsPath,
        target,
    );
    void vscode.window.showInformationMessage(vscode.l10n.t('ParadoxCode: using {0}', selected[0].fsPath));
}

async function chooseGameDirectory(): Promise<void> {
    const selected = await vscode.window.showOpenDialog({
        canSelectFiles: false,
        canSelectFolders: true,
        canSelectMany: false,
        openLabel: vscode.l10n.t('Use EU4 installation for Vanilla data'),
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
        vscode.l10n.t(
            'ParadoxCode: EU4 installation selected. The language server will validate it and build Vanilla data in the background.',
        ),
    );
}

async function exportDiagnostics(): Promise<void> {
    if (!client) {
        void vscode.window.showWarningMessage(vscode.l10n.t('ParadoxCode: the language server is not running.'));
        return;
    }
    try {
        const report = await client.sendRequest<unknown>('pdc/workspaceDiagnostics', {
            offset: 0,
            limit: 128,
        });
        const target = await vscode.window.showSaveDialog({
            saveLabel: vscode.l10n.t('Export Diagnostics'),
            filters: { JSON: ['json'] },
            defaultUri: vscode.Uri.file('paradoxcode-diagnostics.json'),
        });
        if (target) {
            await vscode.workspace.fs.writeFile(target, Buffer.from(JSON.stringify(report, null, 2), 'utf8'));
        }
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: could not export diagnostics: {0}', message));
    }
}

interface WorkspaceFormatSummary {
    totalFiles: number;
    formattedFiles: number;
    unchangedFiles: number;
    skippedUnsafeFiles: number;
    skippedLegacyEncodingFiles: number;
    failedFiles: number;
}

async function formatWorkspace(): Promise<void> {
    if (!client) {
        void vscode.window.showWarningMessage(vscode.l10n.t('ParadoxCode: the language server is not running.'));
        return;
    }
    // Saving first closes the gap between dirty editor buffers and disk: the
    // server rewrites the files on disk, and VSCode reloads clean open
    // documents from disk once those files change.
    await vscode.commands.executeCommand('workbench.action.files.saveAll', false);
    try {
        const summary = await client.sendRequest<WorkspaceFormatSummary>('workspace/executeCommand', {
            command: 'pdc/formatWorkspace',
            arguments: [],
        });
        const skipped = summary.skippedUnsafeFiles + summary.skippedLegacyEncodingFiles;
        const parts = [
            vscode.l10n.t('{0} formatted', summary.formattedFiles),
            vscode.l10n.t('{0} already canonical', summary.unchangedFiles),
        ];
        if (skipped > 0) {
            parts.push(vscode.l10n.t('{0} skipped', skipped));
        }
        if (summary.failedFiles > 0) {
            parts.push(vscode.l10n.t('{0} failed', summary.failedFiles));
        }
        void vscode.window.showInformationMessage(
            vscode.l10n.t(
                'ParadoxCode: workspace formatting finished ({0} script files): {1}.',
                summary.totalFiles,
                parts.join(', '),
            ),
        );
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: workspace formatting failed: {0}', message));
    }
}

async function installServer(context: vscode.ExtensionContext): Promise<boolean> {
    const options = installOptions(context);
    try {
        const binary = await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Notification,
                title: vscode.l10n.t('ParadoxCode: installing pdc'),
                cancellable: false,
            },
            (progress) => installServerRelease(context, options, progress),
        );
        log.appendLine(`pdc ${options.version} installed and verified: ${binary}`);
        void vscode.window.showInformationMessage(vscode.l10n.t('ParadoxCode: pdc {0} is ready.', options.version));
        return true;
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        log.appendLine(`ERROR installing pdc: ${message}`);
        const releasesButton = vscode.l10n.t('Open Releases');
        const openOutputButton = vscode.l10n.t('Open Output');
        const choice = await vscode.window.showErrorMessage(
            vscode.l10n.t('ParadoxCode could not install pdc: {0}', message),
            releasesButton,
            openOutputButton,
        );
        if (choice === releasesButton) {
            await vscode.env.openExternal(vscode.Uri.parse(`https://github.com/${options.repository}/releases`));
        } else if (choice === openOutputButton) {
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
    // `debugLog.dispose` also disposes the wrapped raw channel.
    context.subscriptions.push(log, statusBar, debugLog);
    statusBar.show();
    applyDebugConfiguration();

    loadedFilesProvider = new LoadedFilesProvider();
    context.subscriptions.push(
        loadedFilesProvider,
        vscode.window.registerTreeDataProvider('paradoxcode.loadedFiles', loadedFilesProvider),
        // Agent tools are read-only queries over the shared language-server client; they
        // register whenever the host exposes the Language Model Tools API and stay inert
        // (never invoked) on hosts without a chat provider. The tools are prompt-referenceable
        // (#paradoxSearch, #paradoxValidate, …) so agent sessions can list and enable them.
        ...registerAgentTools(),
    );

    // Transparent localisation (pdcloc:// decoded views) is independent of the
    // language server lifecycle: register it up front so a pdcloc editor can
    // be restored from a previous session even while pdc is still starting.
    void activateTransparentLocalisation(context, log);

    const refresh = debounce(() => {
        if (previewRefreshMode() === 'always') {
            void MissionPreviewPanel.refresh(client);
        }
    }, 150);
    let restartTask = Promise.resolve();
    const restart = () => {
        restartTask = restartTask
            .catch((error: unknown) => {
                const message = error instanceof Error ? error.message : String(error);
                log.appendLine(`WARNING previous pdc restart failed: ${message}`);
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
        vscode.commands.registerCommand(FOLLOWUP_COMPLETION_TRIGGER_COMMAND, triggerFollowupCompletion),
        vscode.commands.registerCommand('paradoxcode.openOutput', () => log.show(true)),
        vscode.commands.registerCommand('paradoxcode.toggleDebug', () => toggleDebugMode()),
        vscode.commands.registerCommand('paradoxcode.openDebugOutput', () => debugLog.show(true)),
        vscode.commands.registerCommand('paradoxcode.selectServer', () => chooseServerPath()),
        vscode.commands.registerCommand('paradoxcode.installServer', async () => {
            if (await installServer(context)) {
                restart();
            }
        }),
        vscode.commands.registerCommand('paradoxcode.selectGameDirectory', () => chooseGameDirectory()),
        vscode.commands.registerCommand('paradoxcode.addDependency', () => addDependency()),
        vscode.commands.registerCommand('paradoxcode.removeDependency', () => removeDependency()),
        vscode.commands.registerCommand(
            'paradoxcode.openDependencySettings',
            () => openDependencySettings(),
        ),
        vscode.commands.registerCommand('paradoxcode.reloadServer', restart),
        vscode.commands.registerCommand('paradoxcode.updateIndexCaches', () => updateIndexCaches(restart)),
        vscode.commands.registerCommand('paradoxcode.exportDiagnostics', () => exportDiagnostics()),
        vscode.commands.registerCommand('paradoxcode.formatWorkspace', () => formatWorkspace()),
        vscode.commands.registerCommand('paradoxcode.refreshLoadedFiles', () => loadedFilesProvider.refresh(client)),
        vscode.commands.registerCommand('paradoxcode.refreshMissionPreview', () => MissionPreviewPanel.refresh(client)),
        vscode.commands.registerCommand('paradoxcode.openMissionIconPicker', () => {
            void MissionIconPickerPanel.show(context.extensionUri);
        }),
        { dispose: () => MissionPreviewPanel.dispose() },
        { dispose: () => MissionIconPickerPanel.dispose() },
    );

    updateMissionContext(vscode.window.activeTextEditor?.document);
    context.subscriptions.push(
        vscode.window.onDidChangeActiveTextEditor((editor) => {
            updateMissionContext(editor?.document);
            refresh();
        }),
        vscode.workspace.onDidSaveTextDocument((document) => {
            if (isMissionDocument(document) && previewRefreshMode() === 'onSave') {
                void MissionPreviewPanel.refresh(client);
            }
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
            } else if (event.affectsConfiguration('paradoxcode.debug')) {
                // Debug mode and its file mirror are applied live; restarting
                // the server would destroy the repro state being observed.
                applyDebugConfiguration();
            } else if (event.affectsConfiguration('paradoxcode.preview')) {
                void MissionPreviewPanel.refresh(client);
            }
        }),
    );

    void startClient(context, loadedFilesProvider);
}

export function deactivate(): Promise<void> {
    MissionPreviewPanel.dispose();
    MissionIconPickerPanel.dispose();
    statusBar.hide();
    return stopClient(loadedFilesProvider);
}
