import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';

/**
 * Debug-output channel that mirrors everything into an append-mode file while
 * `paradoxcode.debug.logFile` resolves to a writable path. Wrapping the
 * channel (instead of touching every call site) lets the language client's
 * protocol trace and the extension's debug handlers share one file sink.
 *
 * Implements `LogOutputChannel` because `LanguageClientOptions.traceOutputChannel`
 * requires it and the protocol tracer logs through the leveled `trace()` call.
 *
 * The file is append-only and never rotated; disabling debug mode closes the
 * stream via `updateTarget(undefined)`.
 */
export class FileTeeDebugChannel implements vscode.LogOutputChannel {
    private stream: fs.WriteStream | undefined;
    private targetPath: string | undefined;
    private brokenTargetWarned = false;

    constructor(
        private readonly inner: vscode.LogOutputChannel,
        private readonly onNotice: (message: string) => void,
    ) {}

    get name(): string {
        return this.inner.name;
    }

    get logLevel(): vscode.LogLevel {
        return this.inner.logLevel;
    }

    get onDidChangeLogLevel(): vscode.Event<vscode.LogLevel> {
        return this.inner.onDidChangeLogLevel;
    }

    /** Points the file mirror at an absolute path, or disables it with `undefined`. */
    updateTarget(resolvedPath: string | undefined): void {
        if (resolvedPath === this.targetPath) {
            return;
        }
        this.closeStream();
        this.targetPath = resolvedPath;
        this.brokenTargetWarned = false;
        if (!resolvedPath) {
            return;
        }
        try {
            fs.mkdirSync(path.dirname(resolvedPath), { recursive: true });
            const stream = fs.createWriteStream(resolvedPath, { flags: 'a' });
            stream.on('error', (error) => this.failTarget(error));
            this.stream = stream;
        } catch (error) {
            this.failTarget(error);
        }
    }

    /** Writes one session separator; a no-op unless the file mirror is active. */
    markSession(detail: string): void {
        if (this.stream) {
            this.mirror(`--- pdc session ${new Date().toISOString()} (${detail}) ---\n`);
        }
    }

    append(value: string): void {
        this.inner.append(value);
        this.mirror(value);
    }

    appendLine(value: string): void {
        this.inner.appendLine(value);
        this.mirror(`${value}\n`);
    }

    trace(message: string): void {
        this.inner.trace(message);
        this.mirrorLeveled('trace', message);
    }

    debug(message: string): void {
        this.inner.debug(message);
        this.mirrorLeveled('debug', message);
    }

    info(message: string): void {
        this.inner.info(message);
        this.mirrorLeveled('info', message);
    }

    warn(message: string): void {
        this.inner.warn(message);
        this.mirrorLeveled('warn', message);
    }

    error(message: string): void {
        this.inner.error(message);
        this.mirrorLeveled('error', message);
    }

    replace(value: string): void {
        this.inner.replace(value);
    }

    clear(): void {
        this.inner.clear();
    }

    show(preserveFocus?: boolean): void;
    show(column?: vscode.ViewColumn, preserveFocus?: boolean): void;
    show(columnOrPreserveFocus?: vscode.ViewColumn | boolean, preserveFocus?: boolean): void {
        // VS Code's runtime implementation type-checks the first argument, so
        // positional forwarding is correct for both documented spellings.
        this.inner.show(
            columnOrPreserveFocus as vscode.ViewColumn | undefined,
            preserveFocus,
        );
    }

    hide(): void {
        this.inner.hide();
    }

    dispose(): void {
        this.closeStream();
        this.inner.dispose();
    }

    private mirror(text: string): void {
        this.stream?.write(text);
    }

    private mirrorLeveled(level: string, message: string): void {
        if (this.stream) {
            this.mirror(`[${level}] ${message}\n`);
        }
    }

    private closeStream(): void {
        this.stream?.end();
        this.stream = undefined;
    }

    private failTarget(error: unknown): void {
        const target = this.targetPath ?? '<unknown>';
        this.closeStream();
        this.targetPath = undefined;
        if (!this.brokenTargetWarned) {
            this.brokenTargetWarned = true;
            const message = error instanceof Error ? error.message : String(error);
            this.onNotice(`WARNING: debug log file "${target}" is not writable (${message}); file mirroring is disabled.`);
        }
    }
}
