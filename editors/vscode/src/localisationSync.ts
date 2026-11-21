import * as vscode from 'vscode';
import { LSPErrorCodes, ResponseError, type LanguageClient, type LanguageClientOptions } from 'vscode-languageclient/node';

// Match pdc's MAX_DOCUMENT_BYTES and MAX_LSP_MESSAGE_BYTES. The binary codec
// is independent of these analysis limits.
const documentLimit = 16 * 1024 * 1024;
const frameLimit = 32 * 1024 * 1024;

interface SizeCheck { version: number; fits: boolean; margin: number }
type SizeCache = WeakMap<vscode.TextDocument, SizeCheck>;

function cachedSize(doc: vscode.TextDocument, cache: SizeCache, event?: vscode.TextDocumentChangeEvent): boolean | undefined {
    const previous = cache.get(doc);
    if (previous && event && previous.version + 1 === doc.version) {
        // Six bytes bound one JSON-escaped UTF-16 unit and also bound its raw UTF-8.
        // Allow for surrogate pairs on both sides of each edit and version digits.
        const delta = event.contentChanges.reduce((n, c) => n + 6 * (c.rangeLength + c.text.length) + 24, 16);
        if (delta < previous.margin) {
            cache.set(doc, { ...previous, version: doc.version, margin: previous.margin - delta });
            return previous.fits;
        }
    }
    return undefined;
}

function fitsDocument(doc: vscode.TextDocument, cache: SizeCache, event?: vscode.TextDocumentChangeEvent): boolean {
    const length = doc.offsetAt(new vscode.Position(doc.lineCount, 0));
    if (length > documentLimit || length * 3 <= documentLimit) return length <= documentLimit;
    const cached = cachedSize(doc, cache, event);
    if (cached !== undefined) return cached;
    const bytes = Buffer.byteLength(doc.getText());
    const fits = bytes <= documentLimit;
    cache.set(doc, { version: doc.version, fits, margin: Math.abs(documentLimit - bytes) });
    return fits;
}

function fitsFrame(doc: vscode.TextDocument, client: LanguageClient, openCache: SizeCache,
    kind: 'open' | 'change', edit?: vscode.TextDocumentChangeEvent): boolean {
    // Small edits need neither a document copy nor a second JSON serialization.
    // Allow six bytes per UTF-16 code unit for control characters and lone surrogates.
    const changes = kind === 'open' ? undefined : edit?.contentChanges;
    const units = changes ? changes.reduce((n, change) => n + change.text.length, 0)
        : doc.offsetAt(new vscode.Position(doc.lineCount, 0));
    if (6 * (doc.uri.toString().length + units) + 160 * (changes?.length ?? 1) + 512 <= frameLimit) return true;
    if (kind === 'open') {
        const cached = cachedSize(doc, openCache, edit);
        if (cached !== undefined) return cached;
    }
    const converter = client.code2ProtocolConverter;
    const method = kind === 'change' ? 'textDocument/didChange' : 'textDocument/didOpen';
    const params = kind === 'change' && edit ? converter.asChangeTextDocumentParams(edit, doc.uri, doc.version)
        : converter.asOpenTextDocumentParams(doc);
    const bytes = Buffer.byteLength(JSON.stringify({ jsonrpc: '2.0', method, params }));
    const fits = bytes <= frameLimit;
    if (!changes) openCache.set(doc, { version: doc.version, fits, margin: Math.abs(frameLimit - bytes) });
    return fits;
}

/** An unsupported overlay must not take down the server or leave a stale open
 * document. A later edit that fits starts a fresh overlay at its current version. */
export function localisationSync(getClient: () => LanguageClient, log: vscode.OutputChannel):
    Pick<NonNullable<LanguageClientOptions['middleware']>, 'didOpen' | 'didChange' | 'sendNotification' | 'sendRequest'> {
    const suspended = new Set<string>();
    const closingForSize = new WeakSet<object>();
    const sizes = new WeakMap<vscode.TextDocument, SizeCheck>();
    const openSizes = new WeakMap<vscode.TextDocument, SizeCheck>();
    function suspend(key: string): void {
        suspended.add(key);
        log.appendLine(`Localisation buffer exceeds language-server sync limits; editing and transcoding are available: ${key}`);
    }
    return {
        async sendNotification(type, next, params) {
            const method = typeof type === 'string' ? type : type.method;
            if (suspended.size && method === 'textDocument/didClose' && params && typeof params === 'object'
                && !closingForSize.has(params)) {
                const uri = (params as { textDocument?: { uri?: string } }).textDocument?.uri;
                // Filter at the transport so the language client still releases its document on close.
                if (uri && suspended.delete(uri)) return;
            }
            return next(type, params);
        },
        async sendRequest(type, params, token, next) {
            const method = typeof type === 'string' ? type : type.method;
            if (suspended.size && method.startsWith('textDocument/')) {
                const uri = (params as { textDocument?: { uri?: string } } | undefined)?.textDocument?.uri;
                if (uri && suspended.has(uri)) throw new ResponseError(LSPErrorCodes.RequestCancelled, 'Document exceeds the analysis sync limits.');
            }
            return next(type, params, token);
        },
        async didOpen(doc, next) {
            if (doc.languageId !== 'localisation') return next(doc);
            const key = doc.uri.toString();
            if (!fitsDocument(doc, sizes) || !fitsFrame(doc, getClient(), openSizes, 'open')) {
                suspend(key);
                return;
            }
            suspended.delete(key);
            return next(doc);
        },
        async didChange(event, next) {
            const doc = event.document;
            if (doc.languageId !== 'localisation') return next(event);
            const key = doc.uri.toString();
            const client = getClient();
            const fits = fitsDocument(doc, sizes, event);
            if (suspended.has(key)) {
                if (fits && fitsFrame(doc, client, openSizes, 'open', event)) {
                    suspended.delete(key);
                    await client.sendNotification('textDocument/didOpen', client.code2ProtocolConverter.asOpenTextDocumentParams(doc));
                }
            } else if (fits && fitsFrame(doc, client, openSizes, 'change', event)) {
                await next(event);
            } else {
                suspend(key);
                const close = client.code2ProtocolConverter.asCloseTextDocumentParams(doc);
                closingForSize.add(close);
                await client.sendNotification('textDocument/didClose', close);
            }
        },
    };
}
