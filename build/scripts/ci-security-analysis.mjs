#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { lstatSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const SHA = /^[0-9a-f]{40}$/;
const DIGEST = /^[0-9a-f]{64}$/;
const ID = /^CISEC-[0-9A-F]{16}$/;
const LANGUAGES = ['cpp', 'rust'];

export class CiAnalysisError extends Error {}

function fail(message) {
  throw new CiAnalysisError(`CI security analysis: ${message}`);
}

function text(value, limit = 600) {
  if (typeof value !== 'string' || !value.trim() || value.length > limit ||
      /[\u0000-\u001f]/.test(value)) fail('invalid analyzer metadata text');
  return value.trim();
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

export function analysisCounts() {
  return { total: 0, high: 0, medium: 0, low: 0, unclassified: 0,
    outsideWorkspace: 0, duplicates: 0, suppressed: 0 };
}

function identity({ language, headSha, ciRunId, ciRunAttempt }) {
  if (!LANGUAGES.includes(language) || !SHA.test(headSha ?? '') ||
      typeof ciRunId !== 'string' || !/^[1-9][0-9]*$/.test(ciRunId) ||
      !Number.isSafeInteger(ciRunAttempt) || ciRunAttempt < 1) fail('invalid analysis identity');
}

function warningId(language, item) {
  return `CISEC-${createHash('sha256').update(canonicalJson([
    language, item.ruleId, item.file, item.startLine, item.endLine,
    item.startColumn, item.endColumn, item.instanceSha256,
  ])).digest('hex').slice(0, 16).toUpperCase()}`;
}

function column(value) {
  if (value === undefined || value === null) return null;
  if (!Number.isSafeInteger(value) || value < 1) fail('invalid SARIF column');
  return value;
}

function repositoryPath(uri, workspace = process.cwd()) {
  if (typeof uri !== 'string' || uri.length > 2000) fail('invalid SARIF location');
  const value = uri.startsWith('file:')
    ? relative(workspace, fileURLToPath(uri)).split(sep).join('/') : decodeURIComponent(uri);
  if (isAbsolute(value) || value.startsWith('/') || value.includes('\\') ||
      /^[A-Za-z]:/.test(value) || /[\u0000-\u001f]/.test(value) ||
      value.split('/').some(part => !part || part === '.' || part === '..')) return null;
  return value;
}

export function compareAnalysisFindings(a, b) {
  return b.securitySeverity - a.securitySeverity || a.file.localeCompare(b.file) ||
    a.startLine - b.startLine || (a.startColumn ?? 0) - (b.startColumn ?? 0) ||
    a.id.localeCompare(b.id);
}

function validateFinding(item, language) {
  if (!item || Object.keys(item).sort().join(',') !==
      'endColumn,endLine,file,helpUri,id,instanceSha256,priority,ruleId,securitySeverity,startColumn,startLine,title' ||
      typeof item.ruleId !== 'string' || !/^[A-Za-z0-9_./-]{1,200}$/.test(item.ruleId) ||
      repositoryPath(item.file) !== item.file || !ID.test(item.id ?? '') ||
      !DIGEST.test(item.instanceSha256 ?? '') ||
      column(item.startColumn) !== item.startColumn || column(item.endColumn) !== item.endColumn ||
      !Number.isSafeInteger(item.startLine) || item.startLine < 1 ||
      !Number.isSafeInteger(item.endLine) || item.endLine < item.startLine ||
      (item.startLine === item.endLine && item.startColumn !== null && item.endColumn !== null &&
        item.endColumn < item.startColumn) ||
      item.id !== warningId(language, item) || !Number.isFinite(item.securitySeverity) ||
      item.securitySeverity < 4 || item.securitySeverity > 10 ||
      item.priority !== (item.securitySeverity >= 7 ? 'high' : 'medium')) fail('invalid analyzer warning');
  text(item.title);
  if (typeof item.helpUri !== 'string' ||
      (item.helpUri && !/^https:\/\/codeql\.github\.com\//.test(item.helpUri))) fail('invalid analyzer help URL');
}

export function collectSarif(sarif, options) {
  identity(options);
  const { language, headSha, ciRunId, ciRunAttempt, workspace = process.cwd() } = options;
  if (sarif?.version !== '2.1.0' || !Array.isArray(sarif.runs) || !sarif.runs.length) fail('invalid CodeQL SARIF');
  const counts = analysisCounts();
  const findings = new Map(), ruleIds = new Set(), versions = new Set();
  for (const run of sarif.runs) {
    if (run.tool?.driver?.name !== 'CodeQL' || !Array.isArray(run.results) ||
        !Array.isArray(run.invocations) || !run.invocations.length ||
        run.invocations.some(item => item.executionSuccessful !== true)) fail('CodeQL did not complete successfully');
    const extensions = run.tool.extensions === undefined ? [] : run.tool.extensions;
    if (!Array.isArray(extensions)) fail('invalid CodeQL rule metadata');
    const rules = new Map();
    for (const component of [run.tool.driver, ...extensions]) {
      if (!component || (component.rules !== undefined && !Array.isArray(component.rules))) fail('invalid CodeQL rule metadata');
      for (const rule of component.rules ?? []) {
        const id = text(rule?.id, 200);
        if (rules.has(id)) fail('ambiguous CodeQL rule metadata');
        rules.set(id, rule);
        ruleIds.add(id);
      }
    }
    if (!rules.size) fail('CodeQL returned no analysis rules');
    versions.add(text(run.tool.driver.semanticVersion ?? run.tool.driver.version, 100));
    for (const result of run.results) {
      counts.total++;
      const rule = rules.get(result.ruleId);
      if (!rule) fail('missing CodeQL rule metadata');
      const rawScore = rule.properties?.['security-severity'];
      const score = typeof rawScore === 'string' && rawScore.trim() !== '' ? Number(rawScore) : rawScore;
      if (!Number.isFinite(score) || score < 0 || score > 10) { counts.unclassified++; continue; }
      if (score < 4) { counts.low++; continue; }
      const location = result.locations?.[0]?.physicalLocation;
      const artifact = location?.artifactLocation;
      const file = repositoryPath(artifact?.uri ?? run.artifacts?.[artifact?.index]?.location?.uri, resolve(workspace));
      if (file === null) { counts.outsideWorkspace++; continue; }
      const startLine = location?.region?.startLine;
      const endLine = location?.region?.endLine ?? startLine;
      const item = {
        ruleId: rule.id, priority: score >= 7 ? 'high' : 'medium', securitySeverity: score,
        file, startLine, endLine, startColumn: column(location?.region?.startColumn),
        endColumn: column(location?.region?.endColumn),
        instanceSha256: createHash('sha256').update(canonicalJson(result)).digest('hex'),
        title: text(rule.shortDescription?.text ?? rule.name ?? rule.id),
        helpUri: typeof rule.helpUri === 'string' && /^https:\/\/codeql\.github\.com\//.test(rule.helpUri) ? rule.helpUri : '',
      };
      item.id = warningId(language, item);
      validateFinding(item, language);
      const previous = findings.get(item.id);
      if (previous) {
        if (canonicalJson(previous) !== canonicalJson(item)) fail('warning identity collision');
        counts.duplicates++;
      } else findings.set(item.id, item);
    }
  }
  const selected = [...findings.values()].sort(compareAnalysisFindings);
  for (const item of selected) counts[item.priority]++;
  if (versions.size !== 1) fail('mixed CodeQL versions in one language artifact');
  return { version: 1, headSha, ciRunId, ciRunAttempt, language, tool: 'CodeQL',
    analysis: { toolVersion: [...versions][0], rulesInSarif: ruleIds.size },
    counts, findings: selected, suppressedFindings: [] };
}

export function validateAnalysisReport(report, expected) {
  identity(expected);
  if (report.version !== 1 || report.tool !== 'CodeQL' ||
      ['headSha', 'ciRunId', 'ciRunAttempt', 'language'].some(key => report[key] !== expected[key]) ||
      !Array.isArray(report.findings) || report.findings.length > 10000) fail('mismatched analyzer artifact');
  text(report.analysis?.toolVersion, 100);
  if (!Number.isSafeInteger(report.analysis?.rulesInSarif) || report.analysis.rulesInSarif < 1) fail('missing analyzer rule coverage');
  const suppressedFindings = report.suppressedFindings === undefined ? [] : report.suppressedFindings;
  if (!Array.isArray(suppressedFindings) || suppressedFindings.length > 10000) fail('invalid suppressed findings');
  const ids = new Set();
  for (const item of [...report.findings, ...suppressedFindings.map(entry => entry.finding)]) {
    validateFinding(item, expected.language);
    if (ids.has(item.id)) fail('duplicate analyzer warning');
    ids.add(item.id);
  }
  for (const entry of suppressedFindings) {
    if (Object.keys(entry).sort().join(',') !== 'finding,reason') fail('invalid suppression evidence');
    text(entry.reason);
  }
  const counts = { ...report.counts, suppressed: report.counts?.suppressed === undefined ? 0 : report.counts.suppressed };
  for (const key of Object.keys(analysisCounts())) {
    if (!Number.isSafeInteger(counts[key]) || counts[key] < 0) fail('invalid warning counts');
  }
  for (const priority of ['high', 'medium']) {
    if (counts[priority] !== report.findings.filter(item => item.priority === priority).length) fail('warning count does not match the artifact');
  }
  if (counts.suppressed !== suppressedFindings.length ||
      counts.high + counts.medium + counts.low + counts.duplicates +
      counts.unclassified + counts.outsideWorkspace + counts.suppressed !== counts.total) fail('warning classifications exceed the total');
  return { ...report, counts, suppressedFindings };
}

export function applySuppressionBaseline(report, baseline, sourceBlob) {
  report = validateAnalysisReport(report, report);
  if (baseline?.version !== 1 || !SHA.test(baseline.reviewedHeadSha ?? '') ||
      !baseline.sourceBlobs || typeof baseline.sourceBlobs !== 'object' ||
      !Array.isArray(baseline.entries) || baseline.entries.length > 10000) fail('invalid suppression baseline');
  for (const [path, blob] of Object.entries(baseline.sourceBlobs)) {
    if (repositoryPath(path) !== path || !SHA.test(blob)) fail('invalid reviewed suppression source');
  }
  const entries = new Map();
  for (const entry of baseline.entries) {
    if (!LANGUAGES.includes(entry.language)) fail('invalid suppression language');
    validateFinding(entry.finding, entry.language);
    text(entry.toolVersion, 100);
    text(entry.reason);
    if (!Array.isArray(entry.sources) || !entry.sources.length || !entry.sources.includes(entry.finding.file) ||
        entry.sources.some(path => !Object.hasOwn(baseline.sourceBlobs, path)) ||
        entries.has(entry.finding.id)) fail('invalid or duplicate suppression source binding');
    entries.set(entry.finding.id, entry);
  }
  const blobs = new Map();
  const counts = { ...report.counts };
  const findings = [], suppressedFindings = [...report.suppressedFindings];
  for (const finding of report.findings) {
    const entry = entries.get(finding.id);
    const accepted = entry && entry.language === report.language &&
      entry.toolVersion === report.analysis.toolVersion &&
      canonicalJson(entry.finding) === canonicalJson(finding) && entry.sources.every(path => {
        if (!blobs.has(path)) blobs.set(path, sourceBlob(path));
        return blobs.get(path) === baseline.sourceBlobs[path];
      });
    if (accepted) {
      counts[finding.priority]--;
      counts.suppressed++;
      suppressedFindings.push({ finding, reason: entry.reason });
    } else findings.push(finding);
  }
  return validateAnalysisReport({ ...report, counts, findings, suppressedFindings }, report);
}

function readJson(path, limit = 16 * 1024 * 1024) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || realpathSync(path) !== resolve(path) || stat.size > limit) fail('expected a bounded regular evidence file');
  return JSON.parse(readFileSync(path, 'utf8'));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const option = name => {
      const index = args.indexOf(name);
      if (index < 0 || !args[index + 1]) fail(`missing ${name}`);
      return args[index + 1];
    };
    if (args[0] !== 'collect') fail('expected collect command');
    const report = collectSarif(readJson(option('--sarif'), 64 * 1024 * 1024), {
      language: option('--language'), headSha: option('--head'),
      ciRunId: process.env.GITHUB_RUN_ID, ciRunAttempt: Number(process.env.GITHUB_RUN_ATTEMPT),
    });
    const result = args.includes('--baseline') ? applySuppressionBaseline(report, readJson(option('--baseline')), path => {
      const tree = execFileSync('git', ['--literal-pathspecs', 'ls-tree', '-z', report.headSha, '--', path],
        { encoding: 'utf8', timeout: 30000, maxBuffer: 1024 * 1024 });
      return /^100(?:644|755) blob ([0-9a-f]{40})\t/.exec(tree)?.[1];
    }) : report;
    writeFileSync(option('--output'), `${JSON.stringify(result, null, 2)}\n`, { flag: 'wx' });
    const escape = value => String(value).replaceAll('%', '%25').replaceAll('\r', '%0D')
      .replaceAll('\n', '%0A').replaceAll(',', '%2C').replaceAll(':', '%3A');
    for (const item of result.findings.slice(0, 50)) {
      console.log(`::warning file=${escape(item.file)},line=${item.startLine}::${escape(item.priority)} ${escape(item.ruleId)}: ${escape(item.title)}`);
    }
    console.log(`CodeQL: ${result.counts.high} active high, ${result.counts.medium} active medium, ${result.counts.suppressed} accepted baseline suppressions; complete counts and evidence retained in findings.json.`);
  } catch (error) {
    console.error(error instanceof CiAnalysisError ? error.message :
      'CI security analysis failed: inspect the analyzer execution and evidence format.');
    process.exitCode = 1;
  }
}
