import assert from 'node:assert/strict';
import { test } from 'node:test';
import { join } from 'node:path';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { handshake } from './lib/diagnosis.mjs';
import { firstPartyRuleMetadata } from './lib/report.mjs';
import { REPOSITORY_ROOT } from './lib/options.mjs';
import { COMPLETION_SAMPLE_PREFIXES, completionPrefixSamples } from './lib/completion-audit.mjs';

function clientWithSummary(summary) {
  const progress = ['begin', 'end'].map((kind) => ({
    method: '$/progress', params: { token: 'vanilla-cache', value: { kind, title: 'Vanilla' } },
  }));
  return {
    request: async (method) => method === 'pdc/workspaceSummary' ? summary : {},
    notify: () => {}, handle: () => {}, next: async () => progress.shift(),
  };
}

const options = {
  workspace: '/tmp/audit-workspace', vanillaSource: '/tmp/audit-source',
  vanillaCache: '/tmp/audit-cache', timeoutMs: 1000, fileTimeoutMs: 1000,
};

test('Vanilla readiness waits for its own progress token through interleaved workspace progress', async () => {
  const events = [
    ['vanilla-cache', 'begin', 'Vanilla index'],
    ['workspace', 'begin', 'Workspace index'],
    ['workspace', 'report', ''],
    ['workspace', 'end', ''],
    ['vanilla-cache', 'end', ''],
  ].map(([token, kind, title]) => ({ method: '$/progress', params: { token, value: { kind, title } } }));
  const client = clientWithSummary({ roots: [{ kind: 'vanilla', path: options.vanillaSource }], fileCounts: { total: 12 } });
  client.next = async () => {
    assert.ok(events.length, 'the matching Vanilla end must be consumed');
    return events.shift();
  };
  await handshake(client, options);
  assert.equal(events.length, 0);
});

test('a completed progress token cannot hide an unindexed or different Vanilla source', async () => {
  for (const summary of [
    { roots: [], fileCounts: { total: 0 } },
    { roots: [{ kind: 'vanilla', path: options.vanillaSource }], fileCounts: { total: 0 } },
    { roots: [{ kind: 'vanilla', path: '/tmp/other-source' }], fileCounts: { total: 12 } },
  ]) {
    await assert.rejects(handshake(clientWithSummary(summary), options), /was not indexed/);
  }
  await handshake(clientWithSummary({
    roots: [{ kind: 'vanilla', path: options.vanillaSource }], fileCounts: { total: 12 },
  }), options);
});

test('audit identity comes from the explicitly selected legacy or IR manifest', () => {
  const directory = mkdtempSync(join(tmpdir(), 'pdc-audit-manifest-'));
  try {
    const path = join(directory, 'historical-manifest.json');
    writeFileSync(path, JSON.stringify({
      source_format_version: 10, rule_hash: 'historical-fixture', semantic_rule_count: 1,
    }));
    const legacy = firstPartyRuleMetadata(path);
    const ir = firstPartyRuleMetadata(join(REPOSITORY_ROOT, 'rules', 'ir-manifest.json'));
    assert.equal(legacy.source_format_version, 10);
    assert.equal(ir.source_format_version, 13);
    assert.notEqual(ir.manifest_rule_hash, legacy.manifest_rule_hash);
    assert.ok(ir.schema_count > 0);
    assert.equal(legacy.semantic_rule_count, 1);
    assert.deepEqual(firstPartyRuleMetadata(), ir);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('completion samples replace the whole token and restore the fixture after request failure', async () => {
  const original = '😀 name = a_token\n';
  const changes = [];
  const version = { value: 1 };
  const client = {
    notify: (_method, params) => changes.push(params),
    request: async (_method, params) => {
      assert.equal(params.position.character, '😀 name = '.length + COMPLETION_SAMPLE_PREFIXES[0].length);
      assert.equal(changes.at(-1).contentChanges[0].text, '😀 name = a\n');
      throw new Error('request failed');
    },
  };
  await assert.rejects(completionPrefixSamples(client, 'file:///test.txt', original,
    { line: 0, character: original.indexOf('_') }, version, 100), /request failed/);
  assert.equal(changes.at(-1).contentChanges[0].text, original);
  assert.equal(changes.at(-1).textDocument.version, 3);
});
