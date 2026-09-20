/**
 * Hand-written stdio MCP endpoint: newline-delimited JSON-RPC 2.0 with the
 * small method surface this server needs (initialize, ping, tools/list,
 * tools/call) plus server-to-client `roots/list`. The framing and dispatch
 * mirror lib/client.mjs's hand-rolled LSP client — no MCP SDK dependency.
 *
 * Only stdout carries protocol traffic; diagnostics go to stderr.
 */

import { appendFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const SUPPORTED_PROTOCOL_VERSIONS = ['2025-06-18', '2025-03-26', '2024-11-05'];
const LATEST_PROTOCOL_VERSION = '2025-06-18';
const JSON_RPC_VERSION = '2.0';

// Env-gated wire trace, same convention as lib/client.mjs.
const TRACE_PATH = process.env.PDC_MCP_TRACE;
function trace(direction, detail) {
  if (!TRACE_PATH) return;
  try {
    appendFileSync(TRACE_PATH, `${Date.now()} ${direction} ${detail}\n`, 'utf8');
  } catch {
    // Tracing is best-effort; never break the transport for it.
  }
}

export function textContent(text) {
  return { content: [{ type: 'text', text }] };
}

export function errorTextContent(error) {
  const message = error instanceof Error ? error.message : String(error);
  return { content: [{ type: 'text', text: `Error: ${message}` }], isError: true };
}

/**
 * One MCP session over a byte transport. `handlers`:
 * - serverInfo: { name, version }
 * - instructions: string surfaced in the initialize result
 * - listTools(): async → MCP tool manifest array
 * - callTool(name, arguments): async → plain string result
 * - onInitialized(): async hook fired after notifications/initialized
 * - onRootsChanged(): async hook fired after notifications/roots/list_changed
 */
export class McpEndpoint {
  constructor(handlers, io = { input: process.stdin, output: process.stdout }) {
    this.handlers = handlers;
    this.io = io;
    this.clientRootsCapability = false;
    this.nextId = 1;
    this.pendingClientRequests = new Map();
    this.closed = false;
    this.initialized = false;
    this.inflightRequests = 0;
  }

  /** Waits until no request is mid-dispatch, bounded by `timeoutMs`. */
  async waitForDrain(timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    while (this.inflightRequests > 0 && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }

  start() {
    this.lines = createInterface({ input: this.io.input, crlfDelay: Infinity });
    this.lines.on('line', (line) => {
      const trimmed = line.trim();
      if (!trimmed) return;
      let message;
      try {
        message = JSON.parse(trimmed);
      } catch (error) {
        this.reply(null, { code: -32700, message: `parse error: ${error.message}` });
        return;
      }
      // Handler errors must not take down the endpoint; receive() isolates them.
      void this.receive(message).catch((error) => {
        console.error(`mcp receive failed: ${error instanceof Error ? error.stack : error}`);
      });
    });
    this.io.input.on('close', () => {
      this.closed = true;
    });
  }

  send(message) {
    if (this.closed) return;
    trace('=>', message.method ?? `response id=${message.id}`);
    this.io.output.write(`${JSON.stringify(message)}\n`);
  }

  reply(id, result) {
    if (id === null || id === undefined) return;
    this.send({ jsonrpc: JSON_RPC_VERSION, id, result });
  }

  replyError(id, error) {
    if (id === null || id === undefined) return;
    this.send({ jsonrpc: JSON_RPC_VERSION, id, error });
  }

  /** Server-to-client request with a bounded wait, used for `roots/list`. */
  requestClient(method, params, timeoutMs) {
    if (this.closed) {
      return Promise.reject(new Error(`cannot send ${method}: transport closed`));
    }
    const id = this.nextId;
    this.nextId += 1;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pendingClientRequests.delete(id);
        reject(new Error(`timed out waiting for the client's ${method} response`));
      }, timeoutMs);
      this.pendingClientRequests.set(id, { method, resolve, reject, timer });
      this.send({ jsonrpc: JSON_RPC_VERSION, id, method, params });
    });
  }

  /** Asks the client for its workspace roots; empty when unsupported. */
  async requestRoots(timeoutMs = 10_000) {
    if (!this.clientRootsCapability) return [];
    try {
      const result = await this.requestClient('roots/list', {}, timeoutMs);
      return Array.isArray(result?.roots) ? result.roots : [];
    } catch (error) {
      console.error(`roots/list unavailable: ${error.message}`);
      return [];
    }
  }

  async receive(message) {
    if (message === null || typeof message !== 'object' || Array.isArray(message)) {
      this.replyError(message?.id, { code: -32600, message: 'invalid request' });
      return;
    }
    if (message.method === undefined) {
      // A response to one of our client-facing requests (roots/list).
      const pending = this.pendingClientRequests.get(message.id);
      if (pending) {
        this.pendingClientRequests.delete(message.id);
        clearTimeout(pending.timer);
        if (message.error) {
          pending.reject(new Error(`${pending.method} failed: ${JSON.stringify(message.error)}`));
        } else {
          pending.resolve(message.result);
        }
      }
      return;
    }
    if (typeof message.method !== 'string' || message.method.length === 0) {
      this.replyError(message.id, { code: -32600, message: 'invalid request method' });
      return;
    }
    // Notifications carry no id. `notifications/cancelled` is acknowledged but
    // not acted on: in-flight LSP requests are bounded by their own timeouts.
    if (message.id === undefined || message.id === null) {
      trace('<=', message.method);
      if (message.method === 'notifications/initialized') {
        this.initialized = true;
        await this.handlers.onInitialized?.();
      } else if (message.method === 'notifications/roots/list_changed') {
        await this.handlers.onRootsChanged?.();
      }
      return;
    }
    trace('<=', `request ${message.id} ${message.method}`);
    this.inflightRequests += 1;
    try {
      const result = await this.dispatch(message.method, message.params);
      this.reply(message.id, result);
    } catch (error) {
      const fail = error?.code ?? -32603;
      this.replyError(message.id, {
        code: fail,
        message: error instanceof Error ? error.message : String(error),
      });
    } finally {
      this.inflightRequests -= 1;
    }
  }

  async dispatch(method, params) {
    switch (method) {
      case 'initialize': {
        const requested = params?.protocolVersion;
        const protocolVersion = SUPPORTED_PROTOCOL_VERSIONS.includes(requested)
          ? requested
          : LATEST_PROTOCOL_VERSION;
        const roots = params?.capabilities?.roots;
        this.clientRootsCapability = roots !== undefined && roots !== null;
        return {
          protocolVersion,
          capabilities: { tools: { listChanged: false } },
          serverInfo: this.handlers.serverInfo,
          instructions: this.handlers.instructions,
        };
      }
      case 'ping':
        return {};
      case 'tools/list':
      case 'tools/call':
        if (!this.initialized) {
          throw Object.assign(
            new Error('server not initialized: send initialize first'),
            { code: -32002 },
          );
        }
        return method === 'tools/list' ? this.listTools() : this.invokeTool(params);
      default:
        throw Object.assign(new Error(`method not found: ${method}`), { code: -32601 });
    }
  }

  async listTools() {
    return { tools: await this.handlers.listTools() };
  }

  async invokeTool(params) {
    const name = params?.name;
    if (typeof name !== 'string' || name.length === 0) {
      throw Object.assign(new Error('tools/call requires a tool name'), { code: -32602 });
    }
    const arguments_ = params?.arguments ?? {};
    if (typeof arguments_ !== 'object' || Array.isArray(arguments_)) {
      throw Object.assign(new Error('tools/call arguments must be an object'), { code: -32602 });
    }
    try {
      return textContent(await this.handlers.callTool(name, arguments_));
    } catch (error) {
      if (error?.code !== undefined) throw error;
      // Operational failures become readable tool results the model can
      // react to (mirrors the extension's register.ts); only protocol
      // level problems surface as JSON-RPC errors.
      console.error(`tool ${name} failed: ${error instanceof Error ? error.message : error}`);
      return errorTextContent(error);
    }
  }
}
