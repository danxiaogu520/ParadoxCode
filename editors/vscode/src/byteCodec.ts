import { spawn } from 'node:child_process';
import * as vscode from 'vscode';
import { acquireServerClient, serverExecutable } from './serverClient';

export type Direction = 'encode' | 'decode';
export type Profile = 'localisation' | 'script';

export class ConversionRefused extends Error {
    constructor(message: string, readonly offset?: number) { super(message); }
}

/** One isolated codec invocation. Neither an index rebuild nor an LSP message size limit
 * can delay a save. Only a completed, successful conversion returns bytes to the caller. */
export async function convertBytes(profile: Profile, direction: Direction, input: Uint8Array): Promise<Buffer> {
    if (!serverExecutable()) await acquireServerClient(30_000);
    const executable = serverExecutable();
    if (!executable) throw new Error(vscode.l10n.t('The ParadoxCode server is unavailable.'));
    return new Promise((resolve, reject) => {
        const child = spawn(executable, ['--transcode-stdio', profile, direction], {
            windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'], timeout: 120_000,
        });
        const output: Buffer[] = [];
        const errors: Buffer[] = [];
        child.stdout.on('data', (bytes: Buffer) => output.push(bytes));
        child.stderr.on('data', (bytes: Buffer) => errors.push(bytes));
        child.on('error', reject);
        child.stdin.on('error', (error: NodeJS.ErrnoException) => {
            if (error.code !== 'EPIPE') reject(error);
        });
        child.on('close', (code, signal) => {
            if (code === 0) {
                resolve(Buffer.concat(output));
                return;
            }
            const detail = Buffer.concat(errors).toString('utf8').trim() || String(signal ?? code);
            if (code === 2) {
                try {
                    const refusal = JSON.parse(detail);
                    if (refusal.refused === 'invalidUtf8') {
                        reject(new ConversionRefused(vscode.l10n.t('Invalid UTF-8 in the localisation file.')));
                        return;
                    }
                    if (refusal.refused === 'brokenEscape') {
                        const offset = refusal.offsets[0];
                        reject(new ConversionRefused(vscode.l10n.t('Damaged escape sequence at byte {0}.', offset), offset));
                        return;
                    }
                    if (refusal.refused === 'unencodable') {
                        const first = refusal.points[0];
                        reject(new ConversionRefused(vscode.l10n.t('U+{0} at byte {1} cannot be encoded for EU4.',
                            first.codePoint.toString(16).toUpperCase(), first.byteIndex), first.byteIndex));
                        return;
                    }
                } catch { /* Report an unexpected response as a codec error below. */ }
            }
            reject(new Error(vscode.l10n.t('Transcoding failed. Verify that the server matches the extension version. {0}', detail)));
        });
        child.stdin.end(input);
    });
}
