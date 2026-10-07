import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export function processGuideSubmission(scope, queued, expected, runtime) {
    if (expected.mode !== 'guide') throw new Error('guide submission requires guide mode');
    let report = null;
    let output = queued;
    if (scope.applicable) {
        if (queued?.items?.length !== 1 || queued.items[0]?.type !== 'add_comment' ||
            typeof queued.items[0].body !== 'string')
            throw new Error('applicable guide scope requires exactly one JSON add_comment');
        try {
            report = JSON.parse(queued.items[0].body);
        } catch {
            throw new Error('guidance comment body must be valid report JSON');
        }
        runtime.validateReport(report, expected);
        // Preserve targeting aliases and envelope errors so the existing gate rejects them.
        output = { ...queued, items: [{ ...queued.items[0], body: runtime.renderReport(report) }] };
    }
    runtime.gatePublication(scope, report, output, expected);
    return {
        report, queued: output,
        verdict: { version: 1, identity: scope.identity, status: report?.status ?? 'pass' },
    };
}

async function main() {
    if (process.argv.length !== 2) throw new Error('guide submission accepts no command or path arguments');
    const runtimePath = process.env.TRUSTED_REVIEW_RUNTIME;
    if (!runtimePath || !path.isAbsolute(runtimePath)) throw new Error('trusted runtime must be an absolute environment path');
    const runtime = await import(pathToFileURL(runtimePath).href);
    const expected = {
        mode: 'guide', prNumber: Number(process.env.PR_NUMBER),
        baseSha: process.env.BASE_SHA, headSha: process.env.HEAD_SHA,
    };
    const queuePath = '/tmp/gh-aw/agent_output.json';
    const stat = fs.lstatSync(queuePath);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size < 2 || stat.size > 2 * 1024 * 1024)
        throw new Error('queued guidance must be a regular JSON file within the 2 MiB size limit');
    const queued = JSON.parse(fs.readFileSync(queuePath, 'utf8'));
    const resultDirectory = '/tmp/gh-aw/performance-result';
    const scope = runtime.prepareScope(expected, resultDirectory);
    const result = processGuideSubmission(scope, queued, expected, runtime);
    const reportJson = `${JSON.stringify(result.report, null, 2)}\n`;
    const queueJson = `${JSON.stringify(result.queued, null, 2)}\n`;
    const verdictJson = `${JSON.stringify(result.verdict, null, 2)}\n`;
    fs.writeFileSync('/tmp/gh-aw/performance-report.json', reportJson, 'utf8');
    fs.writeFileSync(queuePath, queueJson, 'utf8');
    fs.writeFileSync(`${resultDirectory}/performance-verdict.json`, verdictJson, 'utf8');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    main().catch(error => {
        process.stderr.write(`Guidance submission rejected: ${error.message}\n`);
        process.exitCode = 1;
    });
}
