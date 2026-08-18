import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { parse } from 'smol-toml';

/** The shared, editor-agnostic `.pdx/project.toml` `[server]` table. The rest
 * of the file (mod_directory / vanilla_index_cache / dependencies) is consumed
 * by pdx-ls itself via auto-discovery; the extension only needs the server
 * path to launch pdx-ls. */
export interface SharedConfig {
    binary?: string;
}

/** Reads `[server].binary` from the workspace's `.pdx/project.toml`.
 * Returns an empty config when the file is absent; throws (loudly) when the
 * file is present but invalid — the shared config must not be ignored. */
export function readSharedConfig(workspaceFolder: vscode.WorkspaceFolder): SharedConfig {
    const configPath = path.join(workspaceFolder.uri.fsPath, '.pdx', 'project.toml');
    let text: string;
    try {
        text = fs.readFileSync(configPath, 'utf8');
    } catch {
        return {};
    }
    let parsed: unknown;
    try {
        parsed = parse(text);
    } catch (error) {
        throw new Error(
            `invalid .pdx/project.toml: ${error instanceof Error ? error.message : String(error)}`,
        );
    }
    const server = (parsed as { server?: unknown })?.server;
    if (typeof server !== 'object' || server === null) {
        return {};
    }
    const binary = (server as { binary?: unknown }).binary;
    return { binary: typeof binary === 'string' ? binary : undefined };
}
