// Stdio MCP server for ParadoxCode: exposes the extension's five read-only
// EU4 agent tools (validate-text, search-symbols, search-rules,
// search-localisation, hover-info) to any MCP client. Thin shell over
// lib/mcpServer.mjs (protocol), lib/mcpTools.mjs (tool layer), and
// lib/mcpBoot.mjs (server lifecycle and workspace-root resolution).

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  CliUsageError,
  ServerSession,
  USAGE,
  parseArgs,
  resolveServerBinary,
  resolveWorkspaceRoot,
} from './lib/mcpBoot.mjs';
import { McpEndpoint } from './lib/mcpServer.mjs';
import { MCP_INSTRUCTIONS, callTool, readToolManifest } from './lib/mcpTools.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const packageJson = JSON.parse(readFileSync(join(here, '..', 'package.json'), 'utf8'));

let options;
try {
  options = parseArgs(process.argv.slice(2));
} catch (error) {
  if (error instanceof CliUsageError) {
    console.error(error.message);
    process.exit(1);
  }
  throw error;
}
if (options.help) {
  console.error(USAGE);
  process.exit(0);
}
try {
  // Fail fast on an unusable explicit root instead of surfacing it per tool call.
  if (options.workspace) resolveWorkspaceRoot(options.workspace, []);
} catch (error) {
  if (error instanceof CliUsageError) {
    console.error(error.message);
    process.exit(1);
  }
  throw error;
}

let session = undefined;
let bootedRoot = undefined;
let rebootChain = Promise.resolve();

function log(message) {
  console.error(`[paradoxcode-mcp] ${message}`);
}

/** Boots (or re-boots) the language server against the resolved root. */
async function bootSession(root) {
  const next = new ServerSession({
    server: resolveServerBinary(options.server),
    workspace: root.path,
    timeoutMs: options.timeoutMs,
  });
  const previous = session;
  session = next;
  bootedRoot = root.path;
  if (previous) {
    await previous.shutdown();
  }
  log(`workspace root (${root.source}): ${root.path}`);
  // Fire-and-forget: tool calls gate on readiness, boot failures surface there.
  next.boot().catch((error) => {
    log(`server boot failed: ${error.message}`);
  });
}

async function ensureSession() {
  if (session) return session;
  const root = resolveWorkspaceRoot(options.workspace, [], process.cwd());
  if (!root) {
    throw new Error(
      'No workspace root: pass --workspace PATH (or PDC_MCP_WORKSPACE), connect an MCP client '
        + 'that provides roots, or launch this server from the mod directory.',
    );
  }
  await bootSession(root);
  return session;
}

async function resolveRootFromClient(endpoint) {
  const roots = await endpoint.requestRoots();
  return resolveWorkspaceRoot(options.workspace, roots, process.cwd());
}

const endpoint = new McpEndpoint(
  {
    serverInfo: { name: 'paradoxcode-mcp', version: packageJson.version ?? '0' },
    instructions: MCP_INSTRUCTIONS,
    listTools: readToolManifest,
    callTool: async (name, arguments_) => {
      const current = await ensureSession();
      return callTool(name, arguments_, current, bootedRoot);
    },
    onInitialized: async () => {
      const root = await resolveRootFromClient(endpoint);
      if (root) await bootSession(root);
      else log('no workspace root resolved yet; the first tool call will explain how to provide one');
    },
    onRootsChanged: async () => {
      // Serialized re-boot when the client's roots actually move.
      rebootChain = rebootChain.then(async () => {
        const root = await resolveRootFromClient(endpoint);
        if (!root || root.path === bootedRoot) return;
        log(`workspace root changed (${root.source}): ${root.path}`);
        await bootSession(root);
      });
      rebootChain.catch((error) => log(`root change failed: ${error.message}`));
    },
  },
);

let shuttingDown = false;
async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  try {
    // A client that closes stdin may still have requests awaiting responses;
    // let in-flight dispatch finish (bounded) before stopping the server.
    await endpoint.waitForDrain(5_000);
    await session?.shutdown();
  } finally {
    process.exit(0);
  }
}

process.on('SIGINT', () => void shutdown());
process.on('SIGTERM', () => void shutdown());
process.stdin.on('close', () => void shutdown());
process.on('exit', () => {
  // Last-resort cleanup when the graceful path did not run.
  session?.child?.kill();
});

endpoint.start();
