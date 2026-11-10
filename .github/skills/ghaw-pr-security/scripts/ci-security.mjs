#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, realpathSync, renameSync, writeFileSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validatePublicReportText } from './security-review.mjs';

const SHA = /^[0-9a-f]{40}$/;
const ID = /^CISEC-[0-9A-F]{16}$/;
const POSITIVE = /^[1-9][0-9]*$/;
const LANGUAGES = ['cpp', 'rust'];

class CiSecurityContractError extends Error {}

function fail(message) {
  throw new CiSecurityContractError(`CI security: ${message}`);
}

function readJson(path, limit = 16 * 1024 * 1024) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || realpathSync(path) !== resolve(path) || stat.size > limit) {
    fail('expected a bounded regular evidence file');
  }
  return JSON.parse(readFileSync(path, 'utf8'));
}

function text(value, limit = 600) {
  return validatePublicReportText(value, 'CI warning proposal text', limit);
}

function identity(value) {
  if (!value || !SHA.test(value.headSha ?? '') || typeof value.runId !== 'string' || !POSITIVE.test(value.runId) ||
      !Number.isSafeInteger(value.runAttempt) || value.runAttempt < 1) fail('invalid CI identity');
  return value;
}

function warningId(language, ruleId, file, startLine, endLine) {
  return `CISEC-${createHash('sha256').update(
    [language, ruleId, file, startLine, endLine].join('\0')).digest('hex').slice(0, 16).toUpperCase()}`;
}

function repositoryPath(uri, workspace) {
  if (typeof uri !== 'string' || uri.length > 2000) fail('invalid SARIF location');
  let value;
  if (uri.startsWith('file:')) {
    value = relative(workspace, fileURLToPath(uri)).split(sep).join('/');
  } else {
    value = decodeURIComponent(uri);
  }
  if (isAbsolute(value) || value.startsWith('/') || value.includes('\\') ||
      /^[A-Za-z]:/.test(value) || /[\u0000-\u001f]/.test(value) ||
      value.split('/').some(part => !part || part === '.' || part === '..')) return null;
  return value;
}

export function collectSarif(sarif, { language, headSha, ciRunId, ciRunAttempt, workspace = process.cwd() }) {
  identity({ headSha, runId: ciRunId, runAttempt: ciRunAttempt });
  if (!LANGUAGES.includes(language) || sarif?.version !== '2.1.0' ||
      !Array.isArray(sarif.runs) || !sarif.runs.length) fail('invalid CodeQL SARIF');
  const counts = { total: 0, high: 0, medium: 0, low: 0, unclassified: 0, outsideWorkspace: 0, duplicates: 0 };
  const findings = new Map();
  const ruleIds = new Set();
  const versions = new Set();
  for (const run of sarif.runs) {
    if (run.tool?.driver?.name !== 'CodeQL' || !Array.isArray(run.results) ||
        !Array.isArray(run.invocations) ||
        !run.invocations.length || run.invocations.some(item => item.executionSuccessful !== true)) {
      fail('CodeQL did not complete successfully');
    }
    const extensions = run.tool.extensions === undefined ? [] : run.tool.extensions;
    if (!Array.isArray(extensions)) fail('invalid CodeQL rule metadata');
    const rules = new Map();
    for (const component of [run.tool.driver, ...extensions]) {
      if (!component || (component.rules !== undefined && !Array.isArray(component.rules))) {
        fail('invalid CodeQL rule metadata');
      }
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
      if (!Number.isFinite(score) || score < 0 || score > 10) {
        counts.unclassified++;
        continue;
      }
      if (score < 4) {
        counts.low++;
        continue;
      }
      const location = result.locations?.[0]?.physicalLocation;
      const artifact = location?.artifactLocation;
      const uri = artifact?.uri ?? run.artifacts?.[artifact?.index]?.location?.uri;
      const file = repositoryPath(uri, resolve(workspace));
      if (file === null) {
        counts.outsideWorkspace++;
        continue;
      }
      const startLine = location?.region?.startLine;
      const endLine = location?.region?.endLine ?? startLine;
      if (!Number.isSafeInteger(startLine) || startLine < 1 ||
          !Number.isSafeInteger(endLine) || endLine < startLine ||
          !/^[A-Za-z0-9_./-]{1,200}$/.test(rule.id)) fail('invalid warning location or rule');
      const priority = score >= 7 ? 'high' : 'medium';
      const item = {
        id: warningId(language, rule.id, file, startLine, endLine),
        ruleId: rule.id, priority, securitySeverity: score, file, startLine, endLine,
        title: text(rule.shortDescription?.text ?? rule.name ?? rule.id),
        helpUri: typeof rule.helpUri === 'string' && /^https:\/\/codeql\.github\.com\//.test(rule.helpUri)
          ? rule.helpUri : '',
      };
      if (findings.has(item.id)) counts.duplicates++;
      findings.set(item.id, item);
    }
  }
  const selected = [...findings.values()].sort((a, b) =>
    b.securitySeverity - a.securitySeverity || a.file.localeCompare(b.file) || a.startLine - b.startLine);
  for (const item of selected) counts[item.priority]++;
  if (versions.size !== 1) fail('mixed CodeQL versions in one language artifact');
  return { version: 1, headSha, ciRunId, ciRunAttempt, language, tool: 'CodeQL',
    analysis: { toolVersion: [...versions][0], rulesInSarif: ruleIds.size }, counts, findings: selected };
}

export function readCiWarnings(directory, scope) {
  const ci = identity(readJson(resolve(directory, 'identity.json'), 4096));
  if (ci.headSha !== scope.headSha) fail('CI evidence does not match the reviewed head');
  const findings = [];
  const analyzers = [];
  const counts = { total: 0, high: 0, medium: 0, low: 0, unclassified: 0, outsideWorkspace: 0, duplicates: 0 };
  for (const language of LANGUAGES) {
    const report = readJson(resolve(directory, language, 'findings.json'));
    if (report.version !== 1 || report.tool !== 'CodeQL' || report.language !== language ||
        report.headSha !== ci.headSha || report.ciRunId !== ci.runId ||
        report.ciRunAttempt !== ci.runAttempt || !Array.isArray(report.findings) ||
        report.findings.length > 10000) fail('mismatched analyzer artifact');
    text(report.analysis?.toolVersion, 100);
    if (!Number.isSafeInteger(report.analysis?.rulesInSarif) || report.analysis.rulesInSarif < 1) {
      fail('missing analyzer rule coverage');
    }
    analyzers.push({ language, toolVersion: report.analysis.toolVersion, rulesInSarif: report.analysis.rulesInSarif });
    const ids = new Set();
    for (const item of report.findings) {
      const file = repositoryPath(item.file, process.cwd());
      if (Object.keys(item).sort().join(',') !==
          'endLine,file,helpUri,id,priority,ruleId,securitySeverity,startLine,title' ||
          typeof item.ruleId !== 'string' || !/^[A-Za-z0-9_./-]{1,200}$/.test(item.ruleId) ||
          file !== item.file || !ID.test(item.id ?? '') ||
          item.id !== warningId(language, item.ruleId, item.file, item.startLine, item.endLine) ||
          ids.has(item.id) || !Number.isFinite(item.securitySeverity) ||
          item.securitySeverity < 4 || item.securitySeverity > 10 ||
          item.priority !== (item.securitySeverity >= 7 ? 'high' : 'medium') ||
          !Number.isSafeInteger(item.startLine) || item.startLine < 1 ||
          !Number.isSafeInteger(item.endLine) || item.endLine < item.startLine) {
        fail('invalid analyzer warning');
      }
      ids.add(item.id);
      text(item.title);
      if (typeof item.helpUri !== 'string' ||
          (item.helpUri && !/^https:\/\/codeql\.github\.com\//.test(item.helpUri))) fail('invalid analyzer help URL');
      const changed = scope.immutableHunks?.find(entry => entry.path === item.file);
      const onChangedLines = changed?.hunks?.some(hunk => hunk.headCount > 0 &&
        item.startLine <= hunk.headStart + hunk.headCount - 1 && item.endLine >= hunk.headStart) ?? false;
      findings.push({ ...item, language, onChangedLines });
    }
    for (const key of Object.keys(counts)) {
      if (!Number.isSafeInteger(report.counts?.[key]) || report.counts[key] < 0) fail('invalid warning counts');
      counts[key] += report.counts[key];
    }
    for (const priority of ['high', 'medium']) {
      if (report.counts[priority] !== report.findings.filter(item => item.priority === priority).length) {
        fail('warning count does not match the artifact');
      }
    }
    if (report.counts.high + report.counts.medium + report.counts.low + report.counts.duplicates +
        report.counts.unclassified + report.counts.outsideWorkspace !== report.counts.total) {
      fail('warning classifications exceed the total');
    }
  }
  findings.sort((a, b) => b.securitySeverity - a.securitySeverity ||
    a.file.localeCompare(b.file) || a.startLine - b.startLine);
  return { ci, analyzers, counts, findings };
}

function decisionsPath(directory) {
  return resolve(directory, 'warning-dispositions.json');
}

function existingDecisions(directory) {
  try {
    const value = readJson(decisionsPath(directory));
    if (!Array.isArray(value)) fail('invalid saved warning proposals');
    return value;
  } catch (error) {
    if (error.code === 'ENOENT') return [];
    throw error;
  }
}

export function readWarningPage(directory, scope, offset = 0, limit = 20) {
  if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(limit) ||
      limit < 1 || limit > 20) fail('invalid warning page');
  const { ci, analyzers, counts, findings } = readCiWarnings(directory, scope);
  return {
    ci, analyzers, counts, total: findings.length, findings: findings.slice(offset, offset + limit),
    nextOffset: offset + limit < findings.length ? offset + limit : null,
    reviewed: existingDecisions(directory).length,
    caution: 'A warning outside changed lines is not necessarily unrelated. Compare immutable base/head source; all decisions remain proposals for the PR owner or coding agent.',
  };
}

export function proposeWarningDecisions(directory, scope, input) {
  if (typeof input !== 'string' || Buffer.byteLength(input, 'utf8') > 10 * 1024) fail('warning proposals exceed 10 KiB');
  const batch = JSON.parse(input);
  if (!Array.isArray(batch) || !batch.length || batch.length > 20) fail('submit one to twenty warning proposals');
  const { findings } = readCiWarnings(directory, scope);
  const known = new Set(findings.map(item => item.id));
  const seen = new Set();
  const proposals = batch.map(item => {
    if (!item || Object.keys(item).sort().join(',') !== 'decision,id,proposal,reason,relatedToPr' ||
        !known.has(item.id) || seen.has(item.id) ||
        !['fix', 'ignore'].includes(item.decision) ||
        !['introduced', 'existing-related', 'unrelated', 'uncertain'].includes(item.relatedToPr)) fail('invalid or duplicate warning proposal');
    seen.add(item.id);
    return { id: item.id, decision: item.decision, relatedToPr: item.relatedToPr,
      reason: text(item.reason, 500), proposal: text(item.proposal, 600) };
  });
  const saved = new Map(existingDecisions(directory).map(item => [item.id, item]));
  for (const item of proposals) saved.set(item.id, item);
  const destination = decisionsPath(directory);
  const staging = `${destination}.next`;
  writeFileSync(staging, `${JSON.stringify([...saved.values()], null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  renameSync(staging, destination);
  return { accepted: true, reviewed: saved.size, total: known.size, remaining: known.size - saved.size };
}

export function completedWarningReview(directory, scope) {
  const { ci, analyzers, counts, findings } = readCiWarnings(directory, scope);
  const decisions = existingDecisions(directory);
  const byId = new Map(decisions.map(item => [item.id, item]));
  if (decisions.length !== findings.length || byId.size !== findings.length ||
      findings.some(item => !byId.has(item.id))) fail('every reported warning requires a fix-or-ignore proposal');
  for (const item of decisions) {
    if (!item || Object.keys(item).sort().join(',') !== 'decision,id,proposal,reason,relatedToPr' ||
        !['fix', 'ignore'].includes(item.decision) ||
        !['introduced', 'existing-related', 'unrelated', 'uncertain'].includes(item.relatedToPr)) {
      fail('invalid saved warning proposal');
    }
    text(item.reason, 500);
    text(item.proposal, 600);
  }
  return { version: 1, ci, analyzers, counts, findings: findings.map(item => ({ ...item, ...byId.get(item.id) })),
    ownerAction: 'Proposals only. The PR owner or their coding agent decides whether to apply a fix or accept an ignore; this file does not suppress future checks.' };
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
    const result = collectSarif(readJson(option('--sarif'), 64 * 1024 * 1024), {
      language: option('--language'), headSha: option('--head'),
      ciRunId: process.env.GITHUB_RUN_ID, ciRunAttempt: Number(process.env.GITHUB_RUN_ATTEMPT),
    });
    writeFileSync(option('--output'), `${JSON.stringify(result, null, 2)}\n`, { flag: 'wx' });
    for (const item of result.findings.slice(0, 50)) {
      const escape = value => String(value).replaceAll('%', '%25').replaceAll('\r', '%0D')
        .replaceAll('\n', '%0A').replaceAll(',', '%2C').replaceAll(':', '%3A');
      console.log(`::warning file=${escape(item.file)},line=${item.startLine}::${escape(item.priority)} ${escape(item.ruleId)}: ${escape(item.title)}`);
    }
    console.log(`CodeQL: ${result.counts.high} high, ${result.counts.medium} medium; lower/unclassified/outside-workspace counts retained in findings.json.`);
  } catch (error) {
    console.error(error instanceof CiSecurityContractError ? error.message :
      'CI security analysis failed: inspect the analyzer execution and evidence format.');
    process.exitCode = 1;
  }
}
