#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const SCRIPT_EXTENSIONS = new Set([".txt", ".gfx"]);
const SKIPPED_DIRECTORIES = new Set(["crashes", "dumps", "logs", "patchnotes"]);

function usage() {
  return [
    "usage: node scripts/audit-quoted-scripts.mjs --source <directory> [--rules <semantic-rules.json>] [--json]",
    "",
    "Inventories multiline quoted property values without copying their payloads.",
    "The report contains only paths, keys, structural paths, line numbers, sizes, and aggregate counts.",
  ].join("\n");
}

function parseArgs(argv) {
  const options = { source: null, rules: null, json: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--source" || argument === "--rules") {
      const value = argv[index + 1];
      if (!value) throw new Error(`${argument} requires a value`);
      options[argument.slice(2)] = path.resolve(value);
      index += 1;
    } else if (argument === "--json") {
      options.json = true;
    } else if (argument === "--help" || argument === "-h") {
      process.stdout.write(`${usage()}\n`);
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (!options.source) throw new Error("--source is required");
  return options;
}

function filesUnder(root) {
  const files = [];
  const pending = [root];
  while (pending.length > 0) {
    const directory = pending.pop();
    const entries = fs.readdirSync(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const absolute = path.join(directory, entry.name);
      if (entry.isDirectory() && !SKIPPED_DIRECTORIES.has(entry.name.toLowerCase())) {
        pending.push(absolute);
      }
      else if (entry.isFile() && SCRIPT_EXTENSIONS.has(path.extname(entry.name).toLowerCase())) {
        files.push(absolute);
      }
    }
  }
  files.sort();
  return files;
}

function tokenize(source) {
  const tokens = [];
  let offset = 0;
  let line = 1;
  while (offset < source.length) {
    const character = source[offset];
    if (character === "\n") {
      line += 1;
      offset += 1;
      continue;
    }
    if (/\s/u.test(character)) {
      offset += 1;
      continue;
    }
    if (character === "#") {
      while (offset < source.length && source[offset] !== "\n") offset += 1;
      continue;
    }
    if (character === '"') {
      const start = offset;
      const startLine = line;
      let escaped = false;
      offset += 1;
      while (offset < source.length) {
        const current = source[offset];
        if (current === "\n") line += 1;
        if (!escaped && current === '"') {
          offset += 1;
          break;
        }
        if (!escaped && current === "\\") escaped = true;
        else escaped = false;
        offset += 1;
      }
      tokens.push({
        kind: "scalar",
        text: source.slice(start, offset),
        start,
        end: offset,
        line: startLine,
        quoted: true,
        multiline: line > startLine,
      });
      continue;
    }
    if ("{}=".includes(character)) {
      tokens.push({ kind: character, text: character, start: offset, end: offset + 1, line });
      offset += 1;
      continue;
    }
    if ("<>!".includes(character) && source[offset + 1] === "=") {
      tokens.push({
        kind: "=",
        text: source.slice(offset, offset + 2),
        start: offset,
        end: offset + 2,
        line,
      });
      offset += 2;
      continue;
    }
    const start = offset;
    while (
      offset < source.length &&
      !/\s/u.test(source[offset]) &&
      !"{}=\"#".includes(source[offset])
    ) {
      offset += 1;
    }
    tokens.push({
      kind: "scalar",
      text: source.slice(start, offset),
      start,
      end: offset,
      line,
      quoted: false,
      multiline: false,
    });
  }
  return tokens;
}

function inventoryFile(root, file) {
  const source = fs.readFileSync(file, "utf8");
  const tokens = tokenize(source);
  const structuralPath = [];
  const records = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token.kind === "}") {
      structuralPath.pop();
      continue;
    }
    if (token.kind === "{") {
      structuralPath.push("<anonymous>");
      continue;
    }
    if (token.kind !== "scalar" || tokens[index + 1]?.kind !== "=") continue;
    const value = tokens[index + 2];
    if (!value) continue;
    if (value.kind === "{") {
      structuralPath.push(token.text.replaceAll('"', ""));
      index += 2;
      continue;
    }
    if (value.kind === "scalar" && value.quoted && value.multiline) {
      const payload = value.text.slice(1, value.text.endsWith('"') ? -1 : undefined);
      records.push({
        file: path.relative(root, file).replaceAll(path.sep, "/"),
        line: token.line,
        key: token.text.replaceAll('"', ""),
        parentPath: [...structuralPath],
        rawBytes: Buffer.byteLength(value.text),
        escaped: /\\["\\]/u.test(payload),
        scriptLike: /[={}]|\n\s*[A-Za-z0-9_.:-]+\s*[<>=!]/u.test(payload),
      });
      index += 2;
    }
  }
  return records;
}

function scriptedMacroParameters(root) {
  const macros = new Map();
  for (const relative of ["common/scripted_effects", "common/scripted_triggers"]) {
    const directory = path.join(root, relative);
    if (!fs.existsSync(directory)) continue;
    for (const file of filesUnder(directory)) {
      const tokens = tokenize(fs.readFileSync(file, "utf8"));
      for (let index = 0; index + 2 < tokens.length; index += 1) {
        if (
          tokens[index].kind !== "scalar" ||
          tokens[index + 1].kind !== "=" ||
          tokens[index + 2].kind !== "{"
        ) {
          continue;
        }
        const name = tokens[index].text.replaceAll('"', "").toLowerCase();
        let depth = 1;
        const parameters = macros.get(name) ?? new Set();
        index += 3;
        for (; index < tokens.length && depth > 0; index += 1) {
          const token = tokens[index];
          if (token.kind === "{") depth += 1;
          else if (token.kind === "}") depth -= 1;
          if (depth === 0) break;
          for (const match of token.text.matchAll(/\$([A-Za-z0-9_]+)\$/gu)) {
            parameters.add(match[1].toLowerCase());
          }
        }
        macros.set(name, parameters);
      }
    }
  }
  return macros;
}

function quotedRules(rulesPath) {
  if (!rulesPath) return [];
  const rules = JSON.parse(fs.readFileSync(rulesPath, "utf8"));
  const enumPath = path.join(path.dirname(rulesPath), "enum-values.json");
  const enums = fs.existsSync(enumPath) ? JSON.parse(fs.readFileSync(enumPath, "utf8")) : {};
  const result = [];
  for (const rule of rules) {
    if (rule.shape !== "quoted_script") continue;
    const keys = [];
    if (typeof rule.key?.exact === "string") keys.push(rule.key.exact.toLowerCase());
    if (typeof rule.key?.enum === "string") {
      for (const value of enums[rule.key.enum] ?? []) keys.push(value.toLowerCase());
    }
    const parentPath = rule.parent_path ?? [];
    result.push({
      id: rule.id,
      keys,
      parentPath,
      hasDynamicParent: parentPath.some((segment) => /^<[^>]+>$/u.test(segment)),
    });
  }
  return result;
}

function pathMatches(rulePath, actualPath) {
  if (rulePath.length > actualPath.length) return false;
  const offset = actualPath.length - rulePath.length;
  return rulePath.every((expected, index) => {
    const actual = actualPath[offset + index];
    return (/^<[^>]+>$/u.test(expected) && actual.length > 0) || expected.toLowerCase() === actual.toLowerCase();
  });
}

function report(records, rulesPath, macros) {
  const keys = new Map();
  for (const record of records) keys.set(record.key, (keys.get(record.key) ?? 0) + 1);
  const rules = quotedRules(rulesPath);
  const exactQuotedKeys = new Set(rules.flatMap((rule) => rule.keys));
  const classified = records.map((record) => ({
    ...record,
    candidateRuleIds: rules
      .filter(
        (rule) =>
          rule.keys.includes(record.key.toLowerCase()) && pathMatches(rule.parentPath, record.parentPath),
      )
      .map((rule) => rule.id),
    exactParentPathRuleIds: rules
      .filter(
        (rule) =>
          !rule.hasDynamicParent &&
          rule.keys.includes(record.key.toLowerCase()) &&
          pathMatches(rule.parentPath, record.parentPath),
      )
      .map((rule) => rule.id),
    workspaceMacroCandidates: (() => {
      const invocation = record.parentPath.at(-1)?.toLowerCase();
      return invocation && macros.get(invocation)?.has(record.key.toLowerCase())
        ? [invocation]
        : [];
    })(),
  }));
  const sortedKeys = [...keys.entries()].sort(
    (left, right) => right[1] - left[1] || left[0].localeCompare(right[0]),
  );
  return {
    summary: {
      multilineQuotedProperties: records.length,
      scriptLike: records.filter((record) => record.scriptLike).length,
      escaped: records.filter((record) => record.escaped).length,
      distinctKeys: keys.size,
      exactQuotedRuleKeyCandidates: records.filter((record) =>
        exactQuotedKeys.has(record.key.toLowerCase()),
      ).length,
      structuralRuleCandidates: classified.filter((record) => record.candidateRuleIds.length > 0)
        .length,
      exactParentPathRuleCandidates: classified.filter(
        (record) => record.exactParentPathRuleIds.length > 0,
      ).length,
      workspaceMacroCandidates: classified.filter(
        (record) => record.workspaceMacroCandidates.length > 0,
      ).length,
    },
    keys: sortedKeys.map(([key, count]) => ({
      key,
      count,
      hasExactQuotedRuleKey: exactQuotedKeys.has(key.toLowerCase()),
    })),
    records: classified,
  };
}

function markdown(result) {
  const lines = [
    "# Quoted Script Audit",
    "",
    `- Multiline quoted properties: ${result.summary.multilineQuotedProperties}`,
    `- Script-like payloads: ${result.summary.scriptLike}`,
    `- Payloads with quote/backslash escapes: ${result.summary.escaped}`,
    `- Distinct keys: ${result.summary.distinctKeys}`,
    `- Exact quoted-rule key candidates: ${result.summary.exactQuotedRuleKeyCandidates}`,
    `- Key + structural-path rule candidates: ${result.summary.structuralRuleCandidates}`,
    `- Exact parent-path lower bound: ${result.summary.exactParentPathRuleCandidates}`,
    `- Workspace scripted-macro candidates: ${result.summary.workspaceMacroCandidates}`,
    "",
    "These figures are inventory hints, not semantic coverage: context still needs rule review. " +
      "Rules with dynamic `<...>` parent segments are excluded from the exact-path lower bound because a lexical suffix match cannot prove their semantic container. " +
      "Macro candidates come from top-level workspace definitions and parameter tokens, never first-party helper names.",
    "",
    "| Key | Count | Exact quoted rule key |",
    "| --- | ---: | :---: |",
  ];
  for (const entry of result.keys) {
    lines.push(`| \`${entry.key}\` | ${entry.count} | ${entry.hasExactQuotedRuleKey ? "yes" : "no"} |`);
  }
  return `${lines.join("\n")}\n`;
}

try {
  const options = parseArgs(process.argv.slice(2));
  if (!fs.statSync(options.source).isDirectory()) throw new Error("--source must be a directory");
  const records = filesUnder(options.source).flatMap((file) => inventoryFile(options.source, file));
  const result = report(records, options.rules, scriptedMacroParameters(options.source));
  process.stdout.write(options.json ? `${JSON.stringify(result, null, 2)}\n` : markdown(result));
} catch (error) {
  process.stderr.write(`quoted-script audit failed: ${error.message}\n\n${usage()}\n`);
  process.exitCode = 2;
}
