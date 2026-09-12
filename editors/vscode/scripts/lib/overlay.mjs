/**
 * Virtual overlay workspace for whole-Vanilla diagnosis: the diagnosed tree
 * is mirrored under a scratch directory so document URIs never collide with
 * real files, and every path the server reports back is mapped and validated
 * against the overlay root before it reaches the report.
 */

import { mkdirSync, realpathSync } from 'node:fs';
import { join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { LspProtocolError } from './lsp-client.mjs';

export function fileUri(path) {
  return pathToFileURL(path).href;
}

export function pathKey(path) {
  const key = resolve(path);
  return process.platform === 'win32' ? key.toLowerCase() : key;
}

/**
 * Creates the scratch overlay root below the report output directory and
 * returns it with the realpath the LSP workspace must use (Windows temp
 * paths frequently traverse 8.3 or junction aliases).
 */
export function createOverlayWorkspace(outputDir) {
  const root = resolve(outputDir, '.vanilla-overlay-workspace');
  mkdirSync(root, { recursive: true });
  return { root, mod: realpathSync(root) };
}

/**
 * Maps a source-relative path to its overlay document path.
 */
export function overlayPathFor(root, relativePath) {
  return join(root, ...relativePath.split('/'));
}

/**
 * Resolves a `pdc/workspaceDiagnostics` item back to a real path, refusing
 * logical paths that would escape the diagnosed root.
 */
export function diagnosticItemPath(item, root) {
  if (typeof item.logicalPath !== 'string' || !item.logicalPath) {
    return fileURLToPath(item.uri);
  }
  const candidate = resolve(root, ...item.logicalPath.split('/'));
  const rootKey = pathKey(root);
  const candidateKey = pathKey(candidate);
  if (candidateKey !== rootKey && !candidateKey.startsWith(`${rootKey}${sep}`)) {
    throw new LspProtocolError(
      `pdc/workspaceDiagnostics returned a path outside the Current Mod: ${item.logicalPath}`,
    );
  }
  return candidate;
}
