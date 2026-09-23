// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
const { setTimeout: delay } = require('node:timers/promises');

async function main() {
    const request = JSON.parse(fs.readFileSync(0, 'utf8'));
    if (request.workId !== 'compatibility' ||
        request.decisionId !== 'decision-legacy-quoting-1' ||
        request.executorSessionId !== 'simulated-compatibility-executor-1' ||
        request.decision !== 'Preserve legacy double quotes; recheck the unchanged implementation.') {
        throw new Error('Decision does not belong to this compatibility worker');
    }
    const emit = update => console.log(JSON.stringify({
        decisionId: request.decisionId,
        workId: request.workId,
        executorSessionId: request.executorSessionId,
        update,
    }));
    emit({ kind: 'delivered', pid: process.pid });
    await delay(2500);
    emit({ kind: 'acknowledged' });
    await delay(3500);
    emit({ kind: 'checkStarted' });
    await delay(3500);
    const result = spawnSync(process.execPath, ['--test', '--test-reporter=tap', 'test-compatibility.cjs'], {
        cwd: __dirname,
        encoding: 'utf8',
        timeout: 10000,
        maxBuffer: 65536,
        windowsHide: true,
    });
    if (result.error) throw result.error;
    if (result.signal || ![0, 1].includes(result.status)) {
        throw new Error(`Compatibility check could not complete: ${result.signal ?? result.status}`);
    }
    emit({
        kind: 'checkCompleted',
        exitCode: result.status,
        stdout: result.stdout,
        stderr: result.stderr,
    });
}

main().catch(error => {
    console.error(error.stack ?? String(error));
    process.exitCode = 2;
});
