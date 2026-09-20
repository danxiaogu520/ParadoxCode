import * as vscode from 'vscode';
import { State, type LanguageClient } from 'vscode-languageclient/node';

/**
 * The agent tool layer shares the extension's single language-server client instead of
 * spawning its own process. The active instance is published here by the extension's
 * start/stop lifecycle, so tools never capture a stale client across restarts.
 */

let activeClient: LanguageClient | undefined;

/** Publishes the current client. Pass `undefined` while the server is stopped or restarting. */
export function setAgentClient(client: LanguageClient | undefined): void {
    activeClient = client;
}

/** Thrown when no running client is available; tools translate this into retryable text. */
export class AgentServerUnavailableError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'AgentServerUnavailableError';
    }
}

const AVAILABILITY_POLL_MS = 250;

/**
 * Waits for a running client. The extension activates on EU4 workspaces, but the server
 * can still be installing or restarting when an agent session issues its first call.
 */
export async function acquireAgentClient(
    waitMs: number,
    token?: vscode.CancellationToken,
): Promise<LanguageClient> {
    const deadline = Date.now() + waitMs;
    for (;;) {
        if (token?.isCancellationRequested) {
            throw new AgentServerUnavailableError(
                'The request was cancelled before the ParadoxCode server became available.',
            );
        }
        const client = activeClient;
        if (client && client.state === State.Running) {
            return client;
        }
        if (Date.now() >= deadline) {
            throw new AgentServerUnavailableError(
                client
                    ? 'The ParadoxCode language server is starting or restarting; retry shortly.'
                    : 'The ParadoxCode language server is not running. Open an EU4 workspace (or run "ParadoxCode: Reload Language Server") and retry.',
            );
        }
        await new Promise((resolve) => setTimeout(resolve, AVAILABILITY_POLL_MS));
    }
}

/** Races a request against a wall-clock timeout so one slow query cannot stall a chat loop. */
export async function withTimeout<T>(work: Promise<T>, timeoutMs: number, label: string): Promise<T> {
    let timer: NodeJS.Timeout | undefined;
    try {
        return await Promise.race([
            work,
            new Promise<never>((_, reject) => {
                timer = setTimeout(
                    () => reject(new Error(`${label} timed out after ${Math.round(timeoutMs / 1000)}s`)),
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
