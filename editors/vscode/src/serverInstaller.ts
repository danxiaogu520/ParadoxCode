import * as crypto from 'crypto';
import * as fs from 'fs/promises';
import * as fsSync from 'fs';
import * as https from 'https';
import * as path from 'path';
import { execFile } from 'child_process';
import { promisify } from 'util';
import * as vscode from 'vscode';

const execFileAsync = promisify(execFile);

/** The release repository is deliberately explicit: an installer must never follow a user
 * supplied arbitrary URL or silently import a third-party server. */
export const DEFAULT_SERVER_REPOSITORY = 'danxiaogu520/ParadoxCode';
const MAX_CHECKSUM_BYTES = 1_024;
const MAX_ARCHIVE_BYTES = 64 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES = 128 * 1024 * 1024;
const MAX_REDIRECTS = 5;
// GitHub release downloads may spend a while establishing the signed asset URL on slow or
// intermittently connected networks. A single short timeout made a transient stall look like a
// broken release and prevented the installer from ever reaching archive verification.
const DOWNLOAD_TIMEOUT_MS = 90_000;
const MAX_DOWNLOAD_ATTEMPTS = 3;
const DOWNLOAD_RETRY_DELAYS_MS = [1_000, 3_000] as const;

export interface ServerArtifact {
    target: string;
    binary: string;
    extension: 'tar.gz' | 'zip';
}

export interface ServerInstallOptions {
    version: string;
    repository: string;
    installDirectory: string;
}

function platformArtifact(): ServerArtifact {
    if (process.platform === 'win32' && process.arch === 'x64') {
        return { target: 'x86_64-pc-windows-msvc', binary: 'pdx-ls.exe', extension: 'zip' };
    }
    if (process.platform === 'linux' && process.arch === 'x64') {
        return { target: 'x86_64-unknown-linux-gnu', binary: 'pdx-ls', extension: 'tar.gz' };
    }
    if (process.platform === 'linux' && process.arch === 'arm64') {
        return { target: 'aarch64-unknown-linux-gnu', binary: 'pdx-ls', extension: 'tar.gz' };
    }
    if (process.platform === 'darwin' && process.arch === 'x64') {
        return { target: 'x86_64-apple-darwin', binary: 'pdx-ls', extension: 'tar.gz' };
    }
    if (process.platform === 'darwin' && process.arch === 'arm64') {
        return { target: 'aarch64-apple-darwin', binary: 'pdx-ls', extension: 'tar.gz' };
    }
    throw new Error(`ParadoxCode does not publish pdx-ls for ${process.platform}/${process.arch}.`);
}

export function archiveName(version: string, artifact: ServerArtifact): string {
    return `pdx-ls-v${version}-${artifact.target}.${artifact.extension}`;
}

export function releaseAssetUrl(repository: string, version: string, archive: string): string {
    if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) {
        throw new Error('The server repository must be in owner/name form.');
    }
    if (!/^[0-9A-Za-z][0-9A-Za-z.+-]*$/.test(version)) {
        throw new Error('The server version contains unsupported characters.');
    }
    return `https://github.com/${repository}/releases/download/v${version}/${archive}`;
}

export function parseExpectedChecksum(sidecar: string, archive: string): string {
    const line = sidecar
        .split(/\r?\n/)
        .map((value) => value.trim())
        .find((value) => {
            const fields = value.split(/\s+/);
            return fields.length >= 2 && fields[1].replace(/^\*/, '') === archive;
        });
    if (!line) {
        throw new Error(`The checksum sidecar does not mention ${archive}.`);
    }
    const digest = line.split(/\s+/)[0].toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(digest)) {
        throw new Error('The checksum sidecar contains an invalid SHA-256 digest.');
    }
    return digest;
}

function sha256(bytes: Buffer): string {
    return crypto.createHash('sha256').update(bytes).digest('hex');
}

interface DownloadError extends Error {
    statusCode?: number;
}

function isRetryableDownloadError(error: unknown): boolean {
    if (!error || typeof error !== 'object') {
        return false;
    }
    const candidate = error as { code?: unknown; statusCode?: unknown; message?: unknown };
    if (
        typeof candidate.statusCode === 'number'
        && (candidate.statusCode === 408
            || candidate.statusCode === 425
            || candidate.statusCode === 429
            || candidate.statusCode >= 500)
    ) {
        return true;
    }
    if (
        candidate.code === 'ECONNABORTED'
        || candidate.code === 'ECONNRESET'
        || candidate.code === 'ECONNREFUSED'
        || candidate.code === 'EHOSTUNREACH'
        || candidate.code === 'ENETUNREACH'
        || candidate.code === 'EAI_AGAIN'
        || candidate.code === 'ETIMEDOUT'
    ) {
        return true;
    }
    return typeof candidate.message === 'string'
        && candidate.message.startsWith('Timed out downloading ');
}

async function fetchBytesOnce(url: string, maximum: number, label: string, redirects: number): Promise<Buffer> {
    if (redirects > MAX_REDIRECTS) {
        throw new Error(`Too many redirects while downloading ${label}.`);
    }
    const parsed = new URL(url);
    if (parsed.protocol !== 'https:') {
        throw new Error(`Unsupported download protocol for ${label}.`);
    }
    const get = https.get;
    return new Promise<Buffer>((resolve, reject) => {
        const request = get(parsed, (response) => {
            const status = response.statusCode ?? 0;
            if (status >= 300 && status < 400 && response.headers.location) {
                response.resume();
                void fetchBytesOnce(new URL(response.headers.location, parsed).toString(), maximum, label, redirects + 1)
                    .then(resolve, reject);
                return;
            }
            if (status < 200 || status >= 300) {
                response.resume();
                const error: DownloadError = new Error(`Downloading ${label} failed with HTTP ${status}.`);
                error.statusCode = status;
                reject(error);
                return;
            }
            const chunks: Buffer[] = [];
            let size = 0;
            response.on('data', (chunk: Buffer) => {
                size += chunk.length;
                if (size > maximum) {
                    request.destroy(new Error(`Downloaded ${label} exceeds the ${maximum}-byte safety limit.`));
                    return;
                }
                chunks.push(chunk);
            });
            response.on('end', () => resolve(Buffer.concat(chunks)));
            response.on('error', reject);
        });
        request.setTimeout(
            DOWNLOAD_TIMEOUT_MS,
            () => request.destroy(new Error(`Timed out downloading ${label}.`)),
        );
        request.on('error', reject);
    });
}

async function fetchBytes(url: string, maximum: number, label: string): Promise<Buffer> {
    let lastError: unknown;
    for (let attempt = 0; attempt < MAX_DOWNLOAD_ATTEMPTS; attempt += 1) {
        try {
            return await fetchBytesOnce(url, maximum, label, 0);
        } catch (error) {
            lastError = error;
            if (!isRetryableDownloadError(error) || attempt + 1 >= MAX_DOWNLOAD_ATTEMPTS) {
                throw error;
            }
            await new Promise<void>((resolve) => {
                setTimeout(resolve, DOWNLOAD_RETRY_DELAYS_MS[attempt] ?? DOWNLOAD_RETRY_DELAYS_MS.at(-1));
            });
        }
    }
    // The loop either returns or throws. Keep an explicit guard for TypeScript and future edits.
    throw lastError instanceof Error ? lastError : new Error(String(lastError));
}

async function extractArchive(archive: string, destination: string, artifact: ServerArtifact): Promise<void> {
    await fs.mkdir(destination, { recursive: true });
    try {
        const listing = await execFileAsync(
            'tar',
            artifact.extension === 'tar.gz' ? ['-tzf', archive] : ['-tf', archive],
            { windowsHide: true, maxBuffer: 2 * 1024 * 1024 },
        );
        const entries = listing.stdout.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean);
        if (entries.length !== 1 || entries[0].replace(/\/$/, '') !== artifact.binary) {
            throw new Error(`The pdx-ls archive must contain only ${artifact.binary}.`);
        }
        for (const entry of entries) {
            const normalized = entry.replace(/\\/g, '/');
            if (
                normalized.startsWith('/')
                || /^[A-Za-z]:\//.test(normalized)
                || normalized.split('/').some((part) => part === '..')
                || normalized.length > 4_096
            ) {
                throw new Error(`The pdx-ls archive contains an unsafe path: ${entry}`);
            }
        }
        const details = await execFileAsync(
            'tar',
            artifact.extension === 'tar.gz' ? ['-tvzf', archive] : ['-tvf', archive],
            { windowsHide: true, maxBuffer: 2 * 1024 * 1024 },
        );
        if (details.stdout.split(/\r?\n/).some((line) => /^[lh]/.test(line))) {
            throw new Error('The pdx-ls archive must not contain symbolic or hard links.');
        }
        if (artifact.extension === 'tar.gz') {
            await execFileAsync('tar', ['-xzf', archive, '-C', destination], { windowsHide: true });
        } else {
            await execFileAsync('tar', ['-xf', archive, '-C', destination], { windowsHide: true });
        }
    } catch (error) {
        const commandMissing = error && typeof error === 'object' && 'code' in error
            && String((error as { code?: unknown }).code) === 'ENOENT';
        if (artifact.extension !== 'zip' || process.platform !== 'win32' || !commandMissing) {
            const message = error instanceof Error ? error.message : String(error);
            throw new Error(`Could not extract the pdx-ls archive: ${message}`);
        }
        // Windows installations without bsdtar still have PowerShell's archive support. The
        // paths are encoded as single-quoted literals, so a downloaded filename cannot become
        // executable PowerShell syntax.
        const quote = (value: string) => `'${value.replace(/'/g, "''")}'`;
        await execFileAsync('powershell.exe', [
            '-NoProfile',
            '-NonInteractive',
            '-Command',
            `Expand-Archive -LiteralPath ${quote(archive)} -DestinationPath ${quote(destination)} -Force`,
        ], { windowsHide: true });
    }
}

async function findBinary(root: string, binary: string): Promise<string> {
    const queue: Array<{ directory: string; depth: number }> = [{ directory: root, depth: 0 }];
    let visited = 0;
    while (queue.length > 0) {
        const current = queue.shift()!;
        const entries = await fs.readdir(current.directory, { withFileTypes: true });
        for (const entry of entries) {
            visited += 1;
            if (visited > 512) {
                throw new Error('The pdx-ls archive contains too many entries.');
            }
            const candidate = path.join(current.directory, entry.name);
            if (entry.isFile() && entry.name === binary) {
                return candidate;
            }
            if (entry.isDirectory() && current.depth < 3) {
                queue.push({ directory: candidate, depth: current.depth + 1 });
            }
        }
    }
    throw new Error(`The pdx-ls archive did not contain ${binary}.`);
}

async function replaceFile(source: string, target: string): Promise<void> {
    try {
        await fs.rename(source, target);
    } catch (error) {
        // Windows cannot rename over an existing executable. The target is an exact file
        // inside the caller's validated install directory, so removing it is bounded and
        // allows a repaired install to replace a corrupt cache.
        if (process.platform !== 'win32' || !error || typeof error !== 'object' || !('code' in error)
            || !['EEXIST', 'EPERM', 'ENOTEMPTY'].includes(String((error as { code?: unknown }).code))) {
            throw error;
        }
        await fs.rm(target, { force: true });
        await fs.rename(source, target);
    }
}

export function defaultInstallDirectory(context: vscode.ExtensionContext): string {
    return path.join(context.globalStorageUri.fsPath, 'pdx-ls');
}

export function cachedServerPath(
    context: vscode.ExtensionContext,
    options: Pick<ServerInstallOptions, 'version' | 'installDirectory'>,
): string | undefined {
    let artifact: ServerArtifact;
    try {
        artifact = platformArtifact();
    } catch {
        return undefined;
    }
    const root = path.resolve(options.installDirectory || defaultInstallDirectory(context));
    const binary = path.join(root, `v${options.version}`, artifact.target, artifact.binary);
    const checksum = `${binary}.sha256`;
    try {
        const bytes = fsSync.readFileSync(binary);
        const expected = fsSync.readFileSync(checksum, 'utf8').trim().toLowerCase();
        if (bytes.length === 0 || bytes.length > MAX_EXECUTABLE_BYTES || !/^[0-9a-f]{64}$/.test(expected)) {
            return undefined;
        }
        if (sha256(bytes) !== expected) {
            return undefined;
        }
        return binary;
    } catch {
        return undefined;
    }
}

export async function installPdxLs(
    context: vscode.ExtensionContext,
    options: ServerInstallOptions,
    progress?: vscode.Progress<{ message?: string; increment?: number }>,
): Promise<string> {
    const artifact = platformArtifact();
    const archive = archiveName(options.version, artifact);
    const archiveUrl = releaseAssetUrl(options.repository, options.version, archive);
    const checksumUrl = `${archiveUrl}.sha256`;
    const installRoot = path.resolve(options.installDirectory || defaultInstallDirectory(context));
    const installDirectory = path.join(installRoot, `v${options.version}`, artifact.target);
    await fs.mkdir(installRoot, { recursive: true });
    await fs.mkdir(installDirectory, { recursive: true });
    const binaryPath = path.join(installDirectory, artifact.binary);
    const checksumPath = `${binaryPath}.sha256`;
    try {
        const existing = await fs.readFile(binaryPath);
        const expected = await fs.readFile(checksumPath, 'utf8');
        if (existing.length <= MAX_EXECUTABLE_BYTES && sha256(existing) === expected.trim().toLowerCase()) {
            if (process.platform !== 'win32') {
                await fs.chmod(binaryPath, 0o755);
            }
            return binaryPath;
        }
    } catch {
        // A missing or invalid cache is repaired atomically below.
    }

    const temporaryRoot = await fs.mkdtemp(path.join(installRoot, '.download-'));
    const archivePath = path.join(temporaryRoot, archive);
    const extractedPath = path.join(temporaryRoot, 'extracted');
    try {
        progress?.report({ message: `Downloading ${archive}`, increment: 5 });
        const sidecar = await fetchBytes(checksumUrl, MAX_CHECKSUM_BYTES, 'checksum sidecar');
        const expectedArchiveDigest = parseExpectedChecksum(sidecar.toString('utf8'), archive);
        progress?.report({ message: 'Downloading and verifying pdx-ls', increment: 30 });
        const archiveBytes = await fetchBytes(archiveUrl, MAX_ARCHIVE_BYTES, 'server archive');
        if (sha256(archiveBytes) !== expectedArchiveDigest) {
            throw new Error(`The downloaded pdx-ls archive failed SHA-256 verification: ${archive}.`);
        }
        await fs.writeFile(archivePath, archiveBytes);
        progress?.report({ message: 'Extracting pdx-ls', increment: 35 });
        await extractArchive(archivePath, extractedPath, artifact);
        const extractedBinary = await findBinary(extractedPath, artifact.binary);
        const executable = await fs.readFile(extractedBinary);
        if (executable.length === 0 || executable.length > MAX_EXECUTABLE_BYTES) {
            throw new Error('The extracted pdx-ls executable exceeds the safety limit.');
        }
        const executableDigest = sha256(executable);
        const temporaryBinary = path.join(temporaryRoot, artifact.binary);
        const temporaryChecksum = `${temporaryBinary}.sha256`;
        await fs.writeFile(temporaryBinary, executable, { mode: 0o755 });
        await fs.writeFile(temporaryChecksum, `${executableDigest}\n`, 'utf8');
        if (process.platform !== 'win32') {
            await fs.chmod(temporaryBinary, 0o755);
        }
        await replaceFile(temporaryBinary, binaryPath);
        await replaceFile(temporaryChecksum, checksumPath);
        progress?.report({ message: 'pdx-ls installed', increment: 30 });
        return binaryPath;
    } finally {
        await fs.rm(temporaryRoot, { recursive: true, force: true });
    }
}
