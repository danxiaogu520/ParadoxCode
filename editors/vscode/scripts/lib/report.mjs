/**
 * Diagnostic report model and rendering: the JSON report shape (schema
 * version 1), the running summary aggregates, the bounded tool-error log,
 * and the Markdown rendering used for both final and checkpoint reports.
 */

import { mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { REPOSITORY_ROOT } from './options.mjs';

export const MAX_REPORTED_TOOL_ERRORS = 256;
const MAX_SCAN_DEPTH = 64;

function firstPartyRuleMetadata() {
  const manifestPath = join(REPOSITORY_ROOT, 'rules', 'manifest.json');
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    return {
      manifest_path: manifestPath,
      source_format_version: manifest.source_format_version ?? null,
      target_game_version: manifest.target_game_version ?? null,
      manifest_rule_hash: manifest.rule_hash ?? null,
      semantic_rule_count: manifest.semantic_rule_count ?? null,
      file_category_count: manifest.file_category_count ?? null,
      symbol_descriptor_count: manifest.symbol_descriptor_count ?? null,
    };
  } catch (error) {
    return { manifest_path: manifestPath, read_error: error.message };
  }
}

function severityName(value) {
  switch (Number(value)) {
    case 1:
      return 'error';
    case 2:
      return 'warning';
    case 3:
      return 'info';
    case 4:
      return 'hint';
    default:
      return 'unknown';
  }
}

function diagnosticLocation(diagnostic) {
  const start = diagnostic.range?.start || { line: 0, character: 0 };
  const end = diagnostic.range?.end || start;
  return {
    line: Number(start.line) + 1,
    column: Number(start.character) + 1,
    end_line: Number(end.line) + 1,
    end_column: Number(end.character) + 1,
  };
}

function lineExcerpt(text, lineNumber) {
  const lines = text.split(/\r?\n/);
  return lines[lineNumber] ?? '';
}

function normalizeDiagnostic(diagnostic, text) {
  const location = diagnosticLocation(diagnostic);
  return {
    code: diagnostic.code === undefined ? 'unknown' : String(diagnostic.code),
    severity: Number(diagnostic.severity || 1),
    severity_name: severityName(diagnostic.severity),
    message: String(diagnostic.message || ''),
    source: diagnostic.source || 'paradoxcode',
    range: diagnostic.range || null,
    location,
    excerpt: lineExcerpt(text, location.line - 1),
  };
}

export function baseReport(options, files, skippedSymlinks, omittedSymlinks, depthLimitedDirectories) {
  const cacheStat = statSync(options.vanillaCache);
  return {
    schema_version: 1,
    generated_at: new Date().toISOString(),
    status: 'running',
    inputs: {
      game: 'eu4',
      rules: {
        authority: 'embedded first-party rules/eu4 JSON source',
        external_source: false,
        ...firstPartyRuleMetadata(),
      },
      mode: options.vanillaSource ? 'vanilla-source' : 'current-mod',
      source: options.source,
      current_mod: options.vanillaSource ? null : options.mod,
      workspace: options.workspace,
      vanilla_cache: {
        path: options.vanillaCache,
        size_bytes: cacheStat.size,
        modified_at: cacheStat.mtime.toISOString(),
        loaded: false,
        status_message: null,
      },
      server: options.server,
      fail_on: options.failOn,
      shard: { index: options.shardIndex, count: options.shardCount },
    },
    scan: {
      relevant_files_discovered: files.length,
      symlinks_skipped: skippedSymlinks,
      symlinks_skipped_omitted: omittedSymlinks,
      depth_limited_directories: depthLimitedDirectories,
      workspace_diagnostic_files: null,
      next_workspace_diagnostic_offset: 0,
      diagnosable_files_selected: null,
      oversized_files_skipped: [],
      oversized_files_skipped_omitted: 0,
    },
    files: [],
    summary: {
      files_analyzed: 0,
      files_with_diagnostics: 0,
      total_diagnostics: 0,
      errors: 0,
      warnings: 0,
      infos: 0,
      hints: 0,
      by_code: {},
    },
    tool_errors: [],
    tool_errors_omitted: 0,
    server_messages: [],
  };
}

export function addToolError(report, message) {
  if (report.tool_errors.length < MAX_REPORTED_TOOL_ERRORS) {
    report.tool_errors.push(String(message));
  } else {
    report.tool_errors_omitted += 1;
  }
}

export function addFileResult(report, file, text, encoding, diagnostics, root) {
  const normalized = diagnostics.map((diagnostic) => normalizeDiagnostic(diagnostic, text));
  const relativePath = relative(root, file).split(sep).join('/');
  const result = {
    path: relativePath,
    physical_path: file,
    encoding,
    diagnostics: normalized,
  };
  report.files.push(result);
  report.summary.files_analyzed += 1;
  if (normalized.length) report.summary.files_with_diagnostics += 1;
  for (const diagnostic of normalized) {
    report.summary.total_diagnostics += 1;
    const severity = diagnostic.severity_name;
    if (severity === 'error') report.summary.errors += 1;
    else if (severity === 'warning') report.summary.warnings += 1;
    else if (severity === 'info') report.summary.infos += 1;
    else if (severity === 'hint') report.summary.hints += 1;
    const code = diagnostic.code;
    const byCode = report.summary.by_code[code] || { count: 0, errors: 0, warnings: 0, infos: 0, hints: 0 };
    byCode.count += 1;
    if (severity === 'error') byCode.errors += 1;
    else if (severity === 'warning') byCode.warnings += 1;
    else if (severity === 'info') byCode.infos += 1;
    else if (severity === 'hint') byCode.hints += 1;
    report.summary.by_code[code] = byCode;
  }
}

export function shouldFail(report, failOn) {
  if (failOn === 'none') return false;
  if (failOn === 'warning') return report.summary.errors + report.summary.warnings > 0;
  return report.summary.errors > 0;
}

function markdownEscape(value) {
  return String(value).replaceAll('|', '\\|').replaceAll('`', '\\`');
}

function renderMarkdown(report) {
  const { summary } = report;
  const lines = [
    '# Current Mod diagnostic report',
    '',
    `- Status: **${report.status}**`,
    `- Generated: ${report.generated_at}`,
    `- Mode: \`${markdownEscape(report.inputs.mode)}\``,
    `- Source: \`${markdownEscape(report.inputs.source)}\``,
    `- Vanilla cache: \`${markdownEscape(report.inputs.vanilla_cache.path)}\``,
    `- Rules: ${report.inputs.rules.authority}`,
    `- Failure threshold: \`${report.inputs.fail_on}\``,
    '',
    '## Summary',
    '',
    '| Metric | Count |',
    '| --- | ---: |',
    `| Relevant files discovered | ${report.scan.relevant_files_discovered} |`,
    `| Files analyzed | ${summary.files_analyzed} |`,
    `| Files with diagnostics | ${summary.files_with_diagnostics} |`,
    `| Total diagnostics | ${summary.total_diagnostics} |`,
    `| Errors | ${summary.errors} |`,
    `| Warnings | ${summary.warnings} |`,
    `| Info / hints | ${summary.infos + summary.hints} |`,
    '',
    '### Diagnostic codes',
    '',
    '| Code | Count | Errors | Warnings | Info | Hints |',
    '| --- | ---: | ---: | ---: | ---: | ---: |',
  ];
  if (report.inputs.rules.target_game_version) {
    lines.splice(7, 0, `- Rule target game version: ${markdownEscape(report.inputs.rules.target_game_version)}`);
  }
  if (report.inputs.rules.manifest_rule_hash) {
    lines.splice(8, 0, `- Rule manifest hash: \`${markdownEscape(report.inputs.rules.manifest_rule_hash)}\``);
  }
  const codes = Object.keys(summary.by_code).sort();
  if (!codes.length) lines.push('| _(none)_ | 0 | 0 | 0 | 0 | 0 |');
  for (const code of codes) {
    const value = summary.by_code[code];
    lines.push(`| ${markdownEscape(code)} | ${value.count} | ${value.errors} | ${value.warnings} | ${value.infos} | ${value.hints} |`);
  }

  lines.push('', '## Vanilla and scan status', '');
  const vanilla = report.inputs.vanilla_cache;
  lines.push(`- Cache loaded: **${vanilla.loaded ? 'yes' : 'no'}**`);
  if (vanilla.status_message) lines.push(`- Server status: ${markdownEscape(vanilla.status_message)}`);
  if (report.scan.workspace_diagnostic_files !== null) {
    lines.push(
      `- Indexed workspace files diagnosed: ${report.scan.next_workspace_diagnostic_offset}/${report.scan.workspace_diagnostic_files}`,
    );
  }
  if (report.scan.diagnosable_files_selected !== null) {
    lines.push(
      `- Profile-supported Script/Localisation files: ${report.scan.diagnosable_files_selected}`,
    );
  }
  if (report.scan.symlinks_skipped.length || report.scan.symlinks_skipped_omitted) {
    const symlinkCount = report.scan.symlinks_skipped.length + report.scan.symlinks_skipped_omitted;
    lines.push(`- Symlinks skipped: ${symlinkCount}`);
    for (const path of report.scan.symlinks_skipped.slice(0, 20)) lines.push(`  - \`${markdownEscape(path)}\``);
    if (report.scan.symlinks_skipped.length > 20) lines.push(`  - … ${report.scan.symlinks_skipped.length - 20} more`);
    if (report.scan.symlinks_skipped_omitted) lines.push(`  - … ${report.scan.symlinks_skipped_omitted} more omitted by the report bound`);
  }
  if (report.scan.oversized_files_skipped.length || report.scan.oversized_files_skipped_omitted) {
    const oversizedCount =
      report.scan.oversized_files_skipped.length + report.scan.oversized_files_skipped_omitted;
    lines.push(`- Oversized files skipped: ${oversizedCount}`);
    for (const path of report.scan.oversized_files_skipped.slice(0, 20)) {
      lines.push(`  - \`${markdownEscape(path)}\``);
    }
    if (report.scan.oversized_files_skipped_omitted) {
      lines.push(`  - … ${report.scan.oversized_files_skipped_omitted} more omitted by the report bound`);
    }
  }
  if (report.scan.depth_limited_directories) {
    lines.push(`- Directories beyond the ${MAX_SCAN_DEPTH}-level scan bound skipped: ${report.scan.depth_limited_directories}`);
  }
  if (report.tool_errors.length) {
    lines.push('', '## Tool errors', '');
    for (const error of report.tool_errors) lines.push(`- ${markdownEscape(error)}`);
    if (report.tool_errors_omitted) lines.push(`- … ${report.tool_errors_omitted} more omitted by the report bound`);
  }
  if (report.server_messages.length) {
    lines.push('', '## Server messages', '');
    for (const message of report.server_messages) lines.push(`- ${markdownEscape(message)}`);
  }
  if (report.server_stderr) {
    lines.push('', '## Server stderr', '', '```text', report.server_stderr, '```');
  }

  lines.push('', '## Files with diagnostics', '');
  const affected = report.files.filter((file) => file.diagnostics.length);
  if (!affected.length) {
    lines.push('No diagnostics were reported.');
  } else {
    for (const file of affected) {
      lines.push('', `### \`${markdownEscape(file.path)}\` (${file.diagnostics.length})`, '');
      for (const diagnostic of file.diagnostics) {
        const location = diagnostic.location;
        lines.push(
          `- **${diagnostic.severity_name.toUpperCase()}** \`${markdownEscape(diagnostic.code)}\` at ${location.line}:${location.column} — ${markdownEscape(diagnostic.message)}`,
        );
        if (diagnostic.excerpt.trim()) {
          lines.push('', '  ```text', `  ${diagnostic.excerpt.replaceAll('`', '\\`')}`, '  ```');
        }
      }
    }
  }
  const clean = report.files.length - affected.length;
  if (clean > 0) lines.push('', `_${clean} analyzed file(s) had no diagnostics._`);
  return `${lines.join('\n')}\n`;
}

export function writeReports(report, outputDir) {
  mkdirSync(outputDir, { recursive: true });
  report.files.sort((left, right) => left.path.localeCompare(right.path));
  const stamp = report.generated_at.replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  const base = join(outputDir, `current-mod-${stamp}`);
  const jsonPath = `${base}.json`;
  const markdownPath = `${base}.md`;
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  writeFileSync(markdownPath, renderMarkdown(report), 'utf8');
  return { jsonPath, markdownPath };
}

export function collectServerMessages(client, report) {
  for (const message of client?.serverMessages || []) report.server_messages.push(message);
  report.server_messages = [...new Set(report.server_messages)];
}
