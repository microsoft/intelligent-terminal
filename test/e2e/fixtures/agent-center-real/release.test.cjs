const { test } = require('node:test');
const assert = require('node:assert/strict');
const { summarizeChecks } = require('./release.cjs');

test('empty results cannot approve a release', () => {
    assert.deepEqual(summarizeChecks([]), { passed: 0, failed: 0, blocked: 0, releaseReady: false });
});
test('all passing checks approve a release', () => {
    assert.deepEqual(summarizeChecks([{ status: 'passed' }, { status: 'passed' }]),
        { passed: 2, failed: 0, blocked: 0, releaseReady: true });
});
test('failed and blocked checks prevent release', () => {
    assert.deepEqual(summarizeChecks([{ status: 'passed' }, { status: 'failed' }, { status: 'blocked' }]),
        { passed: 1, failed: 1, blocked: 1, releaseReady: false });
});
test('unknown status is an error rather than successful release', () => {
    assert.throws(() => summarizeChecks([{ status: 'unknown' }]));
});
test('non-array input is rejected', () => {
    assert.throws(() => summarizeChecks(null));
});
test('input is not modified', () => {
    const checks = Object.freeze([Object.freeze({ status: 'passed' })]);
    assert.equal(summarizeChecks(checks).releaseReady, true);
});
