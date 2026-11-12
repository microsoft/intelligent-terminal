import { closeSync, lstatSync, openSync, readFileSync, readdirSync, realpathSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { validatePublicReportText } from './security-review.mjs';
import { analysisCounts, compareAnalysisFindings, validateAnalysisReport } from './ci-analysis-contract.mjs';

function fail(message) {
  throw new Error(`CI warning review: ${message}`);
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

export function readCiWarnings(directory, scope) {
  const ci = readJson(resolve(directory, 'identity.json'), 4096);
  if (!ci || !/^[0-9a-f]{40}$/.test(ci.headSha ?? '') ||
      typeof ci.runId !== 'string' || !/^[1-9][0-9]*$/.test(ci.runId) ||
      !Number.isSafeInteger(ci.runAttempt) || ci.runAttempt < 1) fail('invalid CI identity');
  if (ci.headSha !== scope.headSha) fail('CI evidence does not match the reviewed head');
  const findings = [], analyzers = [], suppressedFindings = [];
  const counts = analysisCounts();
  for (const language of readdirSync(directory).filter(name => ['cpp', 'rust'].includes(name)).sort()) {
    const report = validateAnalysisReport(readJson(resolve(directory, language, 'findings.json')), {
      language, headSha: ci.headSha, ciRunId: ci.runId, ciRunAttempt: ci.runAttempt,
    });
    text(report.analysis.toolVersion, 100);
    analyzers.push({ language, toolVersion: report.analysis.toolVersion, rulesInSarif: report.analysis.rulesInSarif });
    for (const item of report.findings) {
      text(item.title);
      const changed = scope.immutableHunks?.find(entry => entry.path === item.file);
      const onChangedLines = changed?.hunks?.some(hunk => hunk.headCount > 0 &&
        item.startLine <= hunk.headStart + hunk.headCount - 1 && item.endLine >= hunk.headStart) ?? false;
      findings.push({ ...item, language, onChangedLines });
    }
    for (const entry of report.suppressedFindings) {
      text(entry.finding.title);
      text(entry.reason);
      suppressedFindings.push({ ...entry, language });
    }
    for (const key of Object.keys(counts)) counts[key] += report.counts[key];
  }
  findings.sort(compareAnalysisFindings);
  return { ci, analyzers, counts, findings, suppressedFindings };
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
    caution: 'analyzers lists only evidence CI published; absent analyses are not clean scans. Suppressed baseline instances remain auditable. A warning outside changed lines is not necessarily unrelated. All active decisions remain proposals for the owner.',
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
  const destination = decisionsPath(directory);
  const lockPath = `${destination}.lock`;
  let lock;
  try {
    lock = openSync(lockPath, 'wx', 0o600);
  } catch (error) {
    if (error.code === 'EEXIST') fail('another proposal batch is in progress; retry this batch');
    throw error;
  }
  try {
    const saved = new Map(existingDecisions(directory).map(item => [item.id, item]));
    for (const item of proposals) saved.set(item.id, item);
    const staging = `${destination}.next`;
    writeFileSync(staging, `${JSON.stringify([...saved.values()], null, 2)}\n`, { flag: 'wx', mode: 0o600 });
    renameSync(staging, destination);
    return { accepted: true, reviewed: saved.size, total: known.size, remaining: known.size - saved.size };
  } finally {
    closeSync(lock);
    unlinkSync(lockPath);
  }
}

export function completedWarningReview(directory, scope) {
  const { ci, analyzers, counts, findings, suppressedFindings } = readCiWarnings(directory, scope);
  const decisions = existingDecisions(directory);
  const byId = new Map(decisions.map(item => [item.id, item]));
  if (decisions.length !== findings.length || byId.size !== findings.length ||
      findings.some(item => !byId.has(item.id))) fail('every reported warning requires a fix-or-ignore proposal');
  for (const item of decisions) {
    if (!item || Object.keys(item).sort().join(',') !== 'decision,id,proposal,reason,relatedToPr' ||
        !['fix', 'ignore'].includes(item.decision) ||
        !['introduced', 'existing-related', 'unrelated', 'uncertain'].includes(item.relatedToPr)) fail('invalid saved warning proposal');
    text(item.reason, 500);
    text(item.proposal, 600);
  }
  return { version: 1, ci, analyzers, counts, suppressedFindings,
    findings: findings.map(item => ({ ...item, ...byId.get(item.id) })),
    ownerAction: 'New warning decisions are proposals only. The PR owner or their coding agent decides whether to apply a fix or accept an ignore; this file does not suppress future checks. Accepted CI baseline suppressions remain separately auditable.' };
}
