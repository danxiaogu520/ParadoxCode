// Representative prefixes expose candidates hidden by the completion cap.
// These are samples, not a proof that the whole domain has been enumerated.
export const COMPLETION_SAMPLE_PREFIXES = Object.freeze([
  'a', 'c', 'g', 'p', 'r', 's', 't', '_',
  'add_', 'has_', 'is_', 'any_', 'random_', 'set_', 'ROOT', 'PREV', 'FROM',
]);

export function completionWordRange(line, character) {
  if (!Number.isSafeInteger(character) || character < 0 || character > line.length) {
    throw new Error('completion probe position lies outside its line');
  }
  const word = /[A-Za-z0-9_\-.@$]/;
  let start = character;
  let end = character;
  while (start > 0 && word.test(line[start - 1])) start -= 1;
  while (end < line.length && word.test(line[end])) end += 1;
  return { start, end };
}

export function completionCandidates(response) {
  const items = Array.isArray(response) ? response : response?.items;
  if (!Array.isArray(items)) throw new Error('textDocument/completion returned an invalid result');
  return {
    candidates: items.map((item) => ({
      label: String(item?.label ?? ''),
      kind: Number.isSafeInteger(item?.kind) ? item.kind : null,
      detail: item?.detail == null ? null : String(item.detail),
    })).sort((left, right) => compareStrings(left.label, right.label)
      || (left.kind ?? -1) - (right.kind ?? -1)
      || compareStrings(left.detail ?? '', right.detail ?? '')),
    labels: [...new Set(items.map((item) => String(item?.label ?? '')))].sort(),
    is_incomplete: !Array.isArray(response) && Boolean(response.isIncomplete),
  };
}

function compareStrings(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

export async function completionPrefixSamples(client, uri, text, probe, version, timeoutMs) {
  const lines = text.split('\n');
  const line = lines[probe.line];
  const range = completionWordRange(line, probe.character);
  const samples = [];
  try {
    for (const prefix of COMPLETION_SAMPLE_PREFIXES) {
      const edited = [...lines];
      edited[probe.line] = line.slice(0, range.start) + prefix + line.slice(range.end);
      client.notify('textDocument/didChange', {
        textDocument: { uri, version: ++version.value },
        contentChanges: [{ text: edited.join('\n') }],
      });
      const position = { line: probe.line, character: range.start + prefix.length };
      const response = await client.request('textDocument/completion', {
        textDocument: { uri }, position,
      }, timeoutMs);
      samples.push({ prefix, position, ...completionCandidates(response) });
    }
  } finally {
    client.notify('textDocument/didChange', {
      textDocument: { uri, version: ++version.value }, contentChanges: [{ text }],
    });
  }
  return samples;
}
