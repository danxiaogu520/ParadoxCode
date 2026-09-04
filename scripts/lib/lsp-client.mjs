/**
 * Minimal stdio JSON-RPC (LSP) client shared by the diagnostic and
 * performance scripts. Owns message framing, request/response correlation,
 * per-request timeouts, server-to-client request acknowledgement, and the
 * stderr capture that both scripts report at the end.
 *
 * Notifications that no waiter is currently interested in are queued and
 * replayed to the next `next()`/`waitFor()` caller, so a script can wait for
 * an event without racing its own request round trips.
 */

import { performance } from 'node:perf_hooks';
import { appendFileSync } from 'node:fs';

const JSON_RPC_VERSION = '2.0';

// Env-gated wire trace shared by the debugging sessions: each line records the
// client-local time, direction, and a compact message summary. Off by default.
const TRACE_PATH = process.env.PDX_LSP_CLIENT_TRACE;
function trace(direction, detail) {
  if (!TRACE_PATH) return;
  try {
    appendFileSync(TRACE_PATH, `${performance.now().toFixed(1)} ${direction} ${detail}\n`, 'utf8');
  } catch {
    // Tracing is best-effort; never break the transport for it.
  }
}

export class LspProtocolError extends Error {
  constructor(message) {
    super(message);
    this.name = 'LspProtocolError';
  }
}

export class LspClient {
  constructor(child, { timeoutMs = 30_000 } = {}) {
    this.child = child;
    this.timeoutMs = timeoutMs;
    this.buffer = Buffer.alloc(0);
    this.messages = [];
    this.waiters = [];
    this.pending = new Map();
    this.closed = false;
    this.nextId = 1;
    this.serverMessages = [];
    this.serverStderr = '';
    this.progress = new Map();
    this.protocolError = undefined;

    child.stdout.on('data', (chunk) => this.consume(chunk));
    child.stdout.on('end', () => this.failWaiters(new LspProtocolError('pdx-ls closed stdout')));
    child.on('error', (error) => this.failWaiters(error));
    child.on('close', (code, signal) => {
      this.closed = true;
      if (code !== 0 || signal) {
        this.failWaiters(
          new LspProtocolError(`pdx-ls exited with code ${code ?? 'unknown'}${signal ? ` (${signal})` : ''}`),
        );
      } else {
        this.failWaiters(new LspProtocolError('pdx-ls exited before the expected response'));
      }
    });
    child.stderr.on('data', (chunk) => {
      this.serverStderr += chunk.toString('utf8');
      if (this.serverStderr.length > 64 * 1024) this.serverStderr = this.serverStderr.slice(-64 * 1024);
    });
  }

  failPending(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  failWaiters(error) {
    for (const waiter of this.waiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.failPending(error);
  }

  consume(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    const separator = Buffer.from('\r\n\r\n');
    while (true) {
      const headerEnd = this.buffer.indexOf(separator);
      if (headerEnd < 0) return;
      const header = this.buffer.subarray(0, headerEnd).toString('ascii');
      const lengthLine = header
        .split(/\r?\n/)
        .find((line) => line.toLowerCase().startsWith('content-length:'));
      const length = lengthLine ? Number(lengthLine.slice(lengthLine.indexOf(':') + 1).trim()) : NaN;
      if (!Number.isSafeInteger(length) || length < 0) {
        this.protocolError = new LspProtocolError(`invalid LSP Content-Length header: ${header}`);
        this.failWaiters(this.protocolError);
        return;
      }
      const payloadStart = headerEnd + separator.length;
      if (this.buffer.length < payloadStart + length) return;
      const payload = this.buffer.subarray(payloadStart, payloadStart + length).toString('utf8');
      this.buffer = this.buffer.subarray(payloadStart + length);
      let message;
      try {
        message = JSON.parse(payload);
      } catch (error) {
        this.protocolError = new LspProtocolError(`invalid JSON from pdx-ls: ${error.message}`);
        this.failWaiters(this.protocolError);
        return;
      }
      this.enqueue(message);
    }
  }

  enqueue(message) {
    // Arrival stamp for the performance script; non-JSON metadata never
    // round-trips because received messages are never re-serialized.
    message._at = performance.now();
    if (message.method === undefined && message.id !== undefined) {
      trace('<=', `response id=${message.id}`);
    } else if (message.method !== undefined) {
      const params = message.params ?? {};
      const detail =
        message.method === 'textDocument/publishDiagnostics'
          ? `publishDiagnostics ${(params.uri ?? '').split('/').pop()} n=${params.diagnostics?.length ?? 0} at=${message._at.toFixed(1)}`
          : message.method;
      trace('<=', `${message.id !== undefined ? `request ${message.id} ` : ''}${detail}`);
    }
    if (message.id !== undefined && message.method === undefined) {
      // Response to one of our requests.
      const pending = this.pending.get(message.id);
      if (pending) {
        this.pending.delete(message.id);
        clearTimeout(pending.timer);
        if (message.error) {
          pending.reject(
            new LspProtocolError(`${pending.method} failed: ${JSON.stringify(message.error)}`),
          );
        } else {
          pending.resolve(message.result);
        }
      }
      return;
    }
    if (message.method && message.id !== undefined) {
      // Server-initiated request; acknowledge it to keep the transport alive.
      this.handle(message);
      return;
    }
    // Notification: route to the oldest waiter or queue it for later.
    const waiter = this.waiters.shift();
    if (waiter) {
      clearTimeout(waiter.timer);
      waiter.resolve(message);
    } else {
      this.messages.push(message);
    }
  }

  handle(message) {
    if (message.method === '$/progress') {
      const token = message.params?.token;
      const value = message.params?.value;
      if (token !== undefined && value) this.progress.set(String(token), value);
    }
    if (message.method === 'window/showMessage' || message.method === 'window/logMessage') {
      const text = message.params?.message;
      if (text) this.serverMessages.push(String(text));
    }
    if (message.method && message.id !== undefined) {
      // The server sends these requests for dynamic watchers and work-done
      // progress. The scripts do not need either feature, but must acknowledge
      // the request to keep the transport live.
      if (message.method === 'client/registerCapability' || message.method === 'window/workDoneProgress/create') {
        this.send({ jsonrpc: JSON_RPC_VERSION, id: message.id, result: null });
      } else {
        this.send({
          jsonrpc: JSON_RPC_VERSION,
          id: message.id,
          error: { code: -32601, message: `unsupported client request: ${message.method}` },
        });
      }
    }
  }

  async next(timeoutMs) {
    if (this.messages.length) return this.messages.shift();
    if (this.closed) throw new LspProtocolError('pdx-ls is no longer running');
    return new Promise((resolveMessage, reject) => {
      const timer = setTimeout(() => {
        const index = this.waiters.findIndex((candidate) => candidate.resolve === resolveMessage);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new LspProtocolError(`timed out after ${timeoutMs} ms waiting for pdx-ls`));
      }, timeoutMs);
      this.waiters.push({ resolve: resolveMessage, reject, timer });
    });
  }

  send(message) {
    if (this.closed || this.child.stdin.destroyed) {
      throw new LspProtocolError('cannot write to stopped pdx-ls');
    }
    const payload = Buffer.from(JSON.stringify(message), 'utf8');
    this.child.stdin.write(`Content-Length: ${payload.length}\r\n\r\n`);
    this.child.stdin.write(payload);
  }

  notify(method, params) {
    trace('=>', method);
    this.send({ jsonrpc: JSON_RPC_VERSION, method, ...(params === undefined ? {} : { params }) });
  }

  request(method, params, timeoutMs) {
    const id = this.nextId;
    this.nextId += 1;
    const effectiveTimeout = timeoutMs ?? this.timeoutMs;
    trace('=>', `request ${id} ${method}`);
    return new Promise((resolveRequest, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new LspProtocolError(`timed out waiting for ${method}`));
      }, effectiveTimeout);
      this.pending.set(id, { method, resolve: resolveRequest, reject, timer });
      try {
        this.send({ jsonrpc: JSON_RPC_VERSION, id, method, params });
      } catch (error) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(error);
      }
    });
  }

  async waitFor(predicate, timeoutMs, description) {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new LspProtocolError(`timed out waiting for ${description}`);
      const message = await this.next(remaining);
      this.handle(message);
      if (predicate(message)) return message;
    }
  }
}
