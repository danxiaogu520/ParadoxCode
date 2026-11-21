import * as fs from 'node:fs/promises';
import { isUtf8 } from 'node:buffer';
import * as vscode from 'vscode';
import { convertBytes, type Direction } from './byteCodec';
import { replaceFile } from './replaceFile';

type Encoding = 'utf8' | 'latin1' | 'windows1252';
// VS Code's Windows-1252 decoder displays its five undefined bytes as U+FFFD.
const high1252 = '€\uFFFD‚ƒ„…†‡ˆ‰Š‹Œ\uFFFDŽ\uFFFD\uFFFD‘’“”•–—˜™š›œ\uFFFDžŸ';

function display(bytes: Buffer, encoding: Encoding): string {
    if (encoding !== 'windows1252') return bytes.toString(encoding);
    return bytes.toString('latin1').replace(/[\x80-\x9f]/g, ch => high1252[ch.charCodeAt(0) - 128]);
}

function asBytes(text: string, encoding: Encoding): Buffer | undefined {
    if (encoding === 'utf8') return Buffer.from(text);
    const bytes = Buffer.allocUnsafe(text.length);
    for (let i = 0; i < text.length; i++) {
        let code = text.charCodeAt(i);
        if (encoding === 'windows1252' && code >= 128) {
            if (code === 0xfffd) return undefined;
            const index = high1252.indexOf(text[i]);
            if (index !== -1) code = 128 + index;
            else if (code <= 159) return undefined;
        }
        if (code > 255) return undefined;
        bytes[i] = code;
    }
    return bytes;
}

// Advance one displayed UTF-8 scalar, including one replacement character per malformed
// subsequence. VS Code columns count UTF-16 units, while game escapes count raw bytes.
function utf8Width(bytes: Buffer, at: number): number {
    const first = bytes[at];
    if (first < 0xc2 || first > 0xf4) return 1;
    const width = first < 0xe0 ? 2 : first < 0xf0 ? 3 : 4;
    const second = bytes[at + 1];
    if (second < (first === 0xe0 ? 0xa0 : first === 0xf0 ? 0x90 : 0x80)
        || second > (first === 0xed ? 0x9f : first === 0xf4 ? 0x8f : 0xbf)) return 1;
    let len = 1;
    while (len < width && (bytes[at + len] & 0xc0) === 0x80) len++;
    return len;
}

function byteOffset(bytes: Buffer, pos: vscode.Position, encoding: Encoding): number {
    let at = 0;
    for (let line = 0; line < pos.line; line++) {
        while (at < bytes.length && bytes[at] !== 10 && bytes[at] !== 13) at++;
        if (at === bytes.length) throw new Error(vscode.l10n.t('The selection no longer matches the file.'));
        if (bytes[at++] === 13 && bytes[at] === 10) at++;
    }
    let column = 0;
    while (column < pos.character && at < bytes.length && bytes[at] !== 10 && bytes[at] !== 13) {
        const width = encoding === 'utf8' ? utf8Width(bytes, at) : 1;
        column += width === 4 ? 2 : 1;
        at += width;
    }
    if (column !== pos.character) throw new Error(vscode.l10n.t('Select complete characters.'));
    return at;
}

function checkBoundary(bytes: Buffer, offset: number): void {
    // Payload bytes can themselves look like markers. Start at the physical line boundary.
    let at = offset;
    while (at > 0 && bytes[at - 1] !== 10 && bytes[at - 1] !== 13) at--;
    while (at < offset) at += bytes[at] >= 16 && bytes[at] <= 19 ? 3 : 1;
    if (at !== offset) throw new Error(vscode.l10n.t('Select the complete EU4 escape sequence.'));
}

function editorEncoding(doc: vscode.TextDocument): Encoding {
    const name = (doc as vscode.TextDocument & { encoding?: string }).encoding
        ?? vscode.workspace.getConfiguration('files', doc.uri).get<string>('encoding', 'utf8');
    switch (name.toLowerCase()) {
        case 'utf8': case 'utf8bom': return 'utf8';
        case 'iso88591': case 'latin1': return 'latin1';
        case 'windows1252': return 'windows1252';
        default: throw new Error(vscode.l10n.t('Reopen the script as UTF-8 or Western European before converting a selection.'));
    }
}

/** In a single-byte editor a user can type Chinese into a buffer containing old triples.
 * Supply readable runs as UTF-8 and carry each existing triple as its original bytes. */
function workingBytes(text: string, encoding: Encoding): Buffer {
    if (encoding === 'utf8') return Buffer.from(text);
    const chunks: Buffer[] = [];
    let start = 0;
    const marker = /[\x10-\x13]/g;
    for (let found = marker.exec(text); found; found = marker.exec(text)) {
        chunks.push(Buffer.from(text.slice(start, found.index)));
        const triple = asBytes(text.slice(found.index, found.index + 3), encoding);
        if (!triple || triple.length !== 3) throw new Error(vscode.l10n.t('Select the complete EU4 escape sequence.'));
        chunks.push(triple);
        start = marker.lastIndex = found.index + 3;
    }
    chunks.push(Buffer.from(text.slice(start)));
    return Buffer.concat(chunks);
}

type DocumentWithEncoding = vscode.TextDocument & { encoding?: string };
const reopen = vscode.workspace.openTextDocument as (
    uri: vscode.Uri, options: { encoding: string }
) => Thenable<vscode.TextDocument>;

async function replaceInNativeEncoding(editor: vscode.TextEditor, disk: Buffer, start: number, end: number,
    converted: Buffer, encoding: 'utf8' | 'latin1'): Promise<void> {
    const doc = await reopen(editor.document.uri, { encoding: encoding === 'latin1' ? 'iso88591' : 'utf8' });
    const prefix = disk.subarray(0, start).toString(encoding);
    const previous = disk.subarray(start, end).toString(encoding);
    const replacement = converted.toString(encoding);
    const target = await vscode.window.showTextDocument(doc, editor.viewColumn);
    if (doc.isDirty || editorEncoding(doc) !== encoding ||
        doc.getText() !== prefix + previous + disk.subarray(end).toString(encoding)) {
        throw new Error(vscode.l10n.t('The selection no longer matches the file.'));
    }
    const span = new vscode.Range(doc.positionAt(prefix.length), doc.positionAt(prefix.length + previous.length));
    if (!await target.edit(edit => edit.replace(span, replacement))) throw new Error(vscode.l10n.t('The selection no longer matches the file.'));
    target.selection = new vscode.Selection(span.start, doc.positionAt(prefix.length + replacement.length));
}

export async function convertSelection(direction: Direction): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.scheme !== 'file' || !/\.txt$/i.test(editor.document.uri.fsPath)
        || !vscode.workspace.getWorkspaceFolder(editor.document.uri) || editor.selection.isEmpty) return;
    try {
        if (editor.selections.length !== 1) throw new Error(vscode.l10n.t('Select one contiguous text range.'));
        const doc = editor.document;
        const range = editor.selection;
        const version = doc.version;
        const wasDirty = doc.isDirty;
        const file = doc.uri.fsPath;
        const encoding = editorEncoding(doc);
        const disk = await fs.readFile(file);
        const bom = encoding === 'utf8' && disk.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf])) ? 3 : 0;
        const body = disk.subarray(bom);
        const text = doc.getText();
        const selected = doc.getText(range);
        const eol = doc.eol === vscode.EndOfLine.CRLF ? '\r\n' : '\n';
        const onDisk = display(body, encoding);
        const normalised = onDisk.replace(/\r\n|\r|\n/g, eol);
        const knownEncoding = !!(doc as DocumentWithEncoding).encoding;
        const losslessDisk = encoding === 'utf8' ? isUtf8(body) : encoding === 'latin1' || !onDisk.includes('\uFFFD');
        let input: Buffer;
        let start = 0, end = 0;
        if (!wasDirty) {
            if (normalised !== text) throw new Error(vscode.l10n.t('The selection no longer matches the file.'));
            start = byteOffset(body, range.start, encoding);
            end = byteOffset(body, range.end, encoding);
            checkBoundary(body, start);
            checkBoundary(body, end);
            input = body.subarray(start, end);
        } else {
            if (!losslessDisk) throw new Error(vscode.l10n.t('Reopen the script in a lossless single-byte encoding before editing these bytes.'));
            if (!knownEncoding || normalised !== onDisk
                || !asBytes(text.slice(0, doc.offsetAt(range.start)), encoding)
                || !asBytes(text.slice(doc.offsetAt(range.end)), encoding)) {
                throw new Error(vscode.l10n.t('Save your edits losslessly before converting this selection.'));
            }
            const linePrefix = doc.lineAt(range.start.line).text.slice(0, range.start.character);
            const precedingBytes = workingBytes(linePrefix, encoding);
            checkBoundary(precedingBytes, precedingBytes.length);
            input = workingBytes(selected, encoding);
            checkBoundary(input, input.length);
        }
        const converted = await convertBytes('script', direction, input);
        if (!disk.equals(await fs.readFile(file)) || wasDirty !== doc.isDirty || doc.version !== version || editor !== vscode.window.activeTextEditor
            || !range.isEqual(editor.selection)) throw new Error(vscode.l10n.t('The selection or file changed. Select the text again.'));
        if (input.equals(converted) && (!wasDirty || direction === 'decode' || asBytes(selected, encoding)?.equals(converted))) return;
        const replacement = display(converted, encoding);
        if (direction === 'decode' && !wasDirty && knownEncoding && encoding !== 'utf8' && normalised === onDisk
            && isUtf8(body.subarray(0, start)) && isUtf8(body.subarray(end)) && !disk.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf]))) {
            await replaceInNativeEncoding(editor, disk, start, end, converted, 'utf8');
            return;
        }
        const outsideIsLossless = losslessDisk || (!wasDirty &&
            asBytes(text.slice(0, doc.offsetAt(range.start)), encoding)?.equals(body.subarray(0, start)) &&
            asBytes(text.slice(doc.offsetAt(range.end)), encoding)?.equals(body.subarray(end)));
        if (knownEncoding && outsideIsLossless && asBytes(replacement, encoding)?.equals(converted) && normalised === onDisk) {
            if (!await editor.edit(edit => edit.replace(range, replacement))) throw new Error(vscode.l10n.t('The selection no longer matches the file.'));
            return;
        }
        // Native byte-preserving editing keeps undo and normal Save even when the game
        // output is not UTF-8. Reopening with an encoding requires a clean document.
        if (direction === 'encode' && !wasDirty && !bom && knownEncoding && normalised === onDisk) {
            await replaceInNativeEncoding(editor, disk, start, end, converted, 'latin1');
            return;
        }
        // A text editor cannot save arbitrary game bytes (e.g. 0xFD in a UTF-8 document).
        // Commit only the selected disk span, keeping a backup and honouring dirty buffers.
        const action = doc.isDirty ? vscode.l10n.t('Save edits and convert selection') : vscode.l10n.t('Write selected bytes');
        const choice = await vscode.window.showWarningMessage(vscode.l10n.t(
            'These bytes require a direct file edit. ParadoxCode will back up the file before replacing the selection.'), action);
        if (choice !== action) return;
        if (!disk.equals(await fs.readFile(file)) || doc.version !== version || editor !== vscode.window.activeTextEditor || !range.isEqual(editor.selection)) {
            throw new Error(vscode.l10n.t('The selection or file changed. Select the text again.'));
        }
        if (doc.isDirty) {
            const buffer = asBytes(text, encoding);
            if (!buffer || !losslessDisk || !await doc.save()) throw new Error(vscode.l10n.t('Save your edits losslessly before converting this selection.'));
            const saved = await fs.readFile(file);
            const lead = encoding === 'utf8' && saved.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf])) ? 3 : 0;
            if (doc.version !== version || doc.isDirty || !saved.subarray(lead).equals(buffer)) throw new Error(vscode.l10n.t('The selection or file changed. Select the text again.'));
            start = byteOffset(buffer, range.start, encoding);
            end = byteOffset(buffer, range.end, encoding);
            await writeSelection(doc, saved, lead + start, lead + end, converted, version);
        } else {
            await writeSelection(doc, disk, bom + start, bom + end, converted, version);
        }
    } catch (error) {
        void vscode.window.showErrorMessage(vscode.l10n.t('ParadoxCode: {0}', error instanceof Error ? error.message : String(error)));
    }
}

async function writeSelection(doc: vscode.TextDocument, original: Buffer, start: number, end: number, converted: Buffer, version: number): Promise<void> {
    const file = doc.uri.fsPath;
    const output = Buffer.concat([original.subarray(0, start), converted, original.subarray(end)]);
    await fs.writeFile(file + '.pre-transcode.bak', original);
    await replaceFile(file, output, async () => {
        if (!original.equals(await fs.readFile(file)) || doc.isDirty || doc.version !== version) {
            throw new Error(vscode.l10n.t('The selection or file changed. Select the text again.'));
        }
    });
    // After a byte-only commit, keep the next ordinary Save lossless as well.
    // On older hosts the encoding API is absent; the disk edit is still exact.
    if ((doc as DocumentWithEncoding).encoding && !doc.isDirty && vscode.window.activeTextEditor?.document === doc) {
        const encoding = isUtf8(output) ? 'utf8' : 'iso88591';
        if ((doc as DocumentWithEncoding).encoding !== encoding) await reopen(doc.uri, { encoding });
    }
}
