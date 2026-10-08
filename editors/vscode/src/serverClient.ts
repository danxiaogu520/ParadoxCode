import * as vscode from 'vscode';
import { State, type LanguageClient } from 'vscode-languageclient/node';

/**
 * Editor features share the extension's single language-server client instead of
 * spawning its own process. The active instance is published here by the extension's
 * start/stop lifecycle, so features never capture a stale client across restarts.
 */

let activeClient: LanguageClient | undefined;

/** Publishes the current client. Pass `undefined` while the server is stopped or restarting. */
export function setServerClient(client: LanguageClient | undefined): void {
    activeClient = client;
}

/** Thrown when no running client is available; editor features report this to the user. */
export class ServerUnavailableError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'ServerUnavailableError';
    }
}

const AVAILABILITY_POLL_MS = 250;

/**
 * Waits for a running client. The extension activates on EU4 workspaces, but the server
 * can still be installing or restarting when an editor feature issues its first request.
 */
export async function acquireServerClient(
    waitMs: number,
    token?: vscode.CancellationToken,
): Promise<LanguageClient> {
    const deadline = Date.now() + waitMs;
    for (;;) {
        if (token?.isCancellationRequested) {
            throw new ServerUnavailableError(
                vscode.l10n.t('The request was cancelled before the ParadoxCode server became available.'),
            );
        }
        const client = activeClient;
        if (client && client.state === State.Running) {
            return client;
        }
        if (Date.now() >= deadline) {
            throw new ServerUnavailableError(
                client
                    ? vscode.l10n.t('The ParadoxCode language server is starting or restarting; retry shortly.')
                    : vscode.l10n.t('The ParadoxCode language server is not running. Open an EU4 workspace (or run "ParadoxCode: Reload Language Server") and retry.'),
            );
        }
        await new Promise((resolve) => setTimeout(resolve, AVAILABILITY_POLL_MS));
    }
}

/** Races a request against a wall-clock timeout so one slow query cannot stall an editor operation. */
export async function withTimeout<T>(work: Promise<T>, timeoutMs: number, label: string): Promise<T> {
    let timer: NodeJS.Timeout | undefined;
    try {
        return await Promise.race([
            work,
            new Promise<never>((_, reject) => {
                timer = setTimeout(
                    () => reject(new Error(vscode.l10n.t('{0} timed out after {1}s', label, Math.round(timeoutMs / 1000)))),
                    timeoutMs,
                );
            }),
        ]);
    } finally {
        if (timer) {
            clearTimeout(timer);
        }
    }
}
