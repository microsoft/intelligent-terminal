// Offline adapter contracts only; mocked protocol replies are not release evidence.
'use strict';
const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { execFileSync } = require('node:child_process');
const report = require(path.join(__dirname, '..', 'fixtures', 'AgentCenterReportJourney.cjs'));

function view() {
    return { work: { kind: 'Work', id: 'work-fixture', projectId: 'project-fixture', version: 2,
        workspaceId: 'workspace-fixture', currentPlanRevision: 0 },
    spec: { goal: report.GOAL, criteria: [{ id: 'report', description: 'Report exists', evidenceRule: 'artifact:report' }] } };
}
function worker() {
    return { id: 'invocation-fixture', dispatch: {
        kind: 'ProduceResult', id: 'dispatch-fixture', workId: 'work-fixture', taskId: 'task-fixture', taskRevision: 1,
        attemptId: 'attempt-fixture', workspaceId: 'workspace-fixture', role: 'Integration',
        objective: report.GOAL, scope: ['report.txt'], gateDefinitions: [], inputManifestDigest: 'input-fixture'
    } };
}
function taskView() {
    return { task: { id: 'task-fixture', revision: 1 }, attempt: {
        id: 'attempt-fixture', invocationId: 'invocation-fixture', dispatchId: 'dispatch-fixture',
        mode: 'ExclusiveWrite', reservationHeld: true, acknowledged: true
    } };
}
function ownedDirectories(long = false) {
    const root = path.join(__dirname, '..', 'artifacts', `report-unit-${randomUUID()}`);
    const state = long ? path.join(root, 'long-state-'.repeat(8), 'owned-segment-'.repeat(8), 'state-港口') : path.join(root, 'state');
    const directory = path.join(state, 'workspaces', 'workspace-fixture');
    fs.mkdirSync(directory, { recursive: true });
    return { root, state, directory, workspace: {
        id: 'workspace-fixture', workId: 'work-fixture', status: 'Ready', writer: 'None', localRoot: directory
    } };
}

function shortDirectory(directory) {
    const encoded = Buffer.from(path.toNamespacedPath(directory), 'utf16le').toString('base64');
    const script = String.raw`
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class OwnedPath {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern uint GetShortPathNameW(string path, StringBuilder buffer, uint size);
}
'@
$path = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('${encoded}'))
$size = [OwnedPath]::GetShortPathNameW($path, $null, 0)
if (!$size) { throw 'Owned directory short-name lookup failed.' }
$buffer = [Text.StringBuilder]::new([int]$size)
$written = [OwnedPath]::GetShortPathNameW($path, $buffer, $size)
if (!$written -or $written -ge $size) { throw 'Owned directory short-name read failed.' }
[Console]::Write(($buffer.ToString() | ConvertTo-Json -Compress -EscapeHandling EscapeNonAscii))
`;
    return JSON.parse(execFileSync('pwsh.exe', ['-NoProfile', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')],
        { encoding: 'utf8', timeout: 15000, windowsHide: true }));
}

test('keeps one required report artifact and does not invent an executable check', () => {
    const plan = report.makePlan(view(), { workerCapabilityId: 'ite2e-local-report' });
    assert.equal(plan.tasks.length, 1); assert.deepEqual(plan.edges, []);
    assert.deepEqual(plan.tasks[0].gateDefinitions, []);
    assert.deepEqual(plan.tasks[0].criteria[0].requiredEvidence, ['artifact:report']);
    assert.deepEqual(plan.tasks[0].outputs, [{ slot: 'report', kind: 'Report', required: true }]);
    const changed = view(); changed.spec.criteria[0].evidenceRule = 'command:real-check';
    assert.throws(() => report.makePlan(changed, {}), /cannot weaken/);
});

test('requires the explicitly started work and exact report dispatch contract', () => {
    const expected = { id: 'work-fixture', projectId: 'project-fixture', workspaceId: 'workspace-fixture' };
    assert.equal(report.validateInvocation(worker(), expected).id, 'invocation-fixture');
    assert.throws(() => report.validateInvocation(worker(), { ...expected, id: 'another-work' }), /outside/);
    const wrong = worker(); wrong.dispatch.kind = 'EvaluateGate';
    assert.throws(() => report.validateInvocation(wrong, expected), /dispatch/);
});

test('records actual plan-shaped guards and the applied command identity', async () => {
    const calls = [], current = view();
    await report.runReportTurn({ coordinationInput: { scope: { workId: current.work.id }, turnId: 'turn-fixture' } }, {
        call: async (name, params, ifMatch) => {
            calls.push({ name, params, ifMatch });
            if (name === 'work_get') return { data: current };
            if (name === 'project_get') return { data: { workerCapabilityId: 'ite2e-local-report' } };
            if (name === 'plan_propose') return { data: { proposalId: 'proposal-fixture', version: 1 } };
            return { status: 'ok', data: {} };
        },
        lastCommandId: () => 'applied-command-fixture',
        evidence: () => {}
    });
    assert.deepEqual(calls.map(c => c.name), ['work_get', 'project_get', 'plan_propose', 'work_get', 'plan_apply', 'coordination_finish']);
    assert.equal(calls[2].ifMatch[0].version, 2);
    assert.deepEqual(calls[4].ifMatch[1], { kind: 'PlanProposal', id: 'proposal-fixture', version: 1 });
    assert.deepEqual(calls[5].params.commandIds, ['applied-command-fixture']);
});

test('writes only the acknowledged workspace then captures and submits those fixed bytes', async () => {
    const owned = ownedDirectories(true), calls = [];
    try {
        assert.ok(owned.directory.length > 300);
        await report.runReportTurn(worker(), {
            stateRoot: path.toNamespacedPath(owned.state), sessionDirectory: path.toNamespacedPath(owned.directory),
            readWorkspace: () => owned.workspace,
            call: async (name, params) => {
                calls.push({ name, params });
                if (name === 'task_get') return { data: taskView() };
                if (name === 'artifact_capture') {
                    assert.equal(fs.readFileSync(path.join(owned.directory, 'report.txt'), 'utf8'), report.CONTENT);
                    return { data: { artifacts: [{ artifactId: 'artifact-fixture', digest: 'digest-fixture' }] } };
                }
                return { status: 'ok', data: {} };
            },
            evidence: () => {}
        });
        assert.deepEqual(calls.map(c => c.name), ['task_acknowledge', 'task_get', 'artifact_capture', 'result_submit']);
        assert.deepEqual(calls[2].params.sources, [{ kind: 'File', relativePath: 'report.txt' }]);
        assert.equal(calls[3].params.outputs[0].artifact.artifactId, 'artifact-fixture');
        assert.equal(calls[3].params.inputManifestDigest, 'input-fixture');
        assert.deepEqual(fs.readdirSync(owned.directory), ['report.txt']);
    } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
});

for (const long of [false, true]) {
    test(`resolves normal and verbatim ${long ? 'over-300-character' : 'ordinary'} workspace spellings without weakening equality`, t => {
        const owned = ownedDirectories(long);
        try {
            const expected = fs.realpathSync.native(owned.directory);
            for (const state of [owned.state, path.toNamespacedPath(owned.state)]) {
                for (const localRoot of [owned.directory, path.toNamespacedPath(owned.directory)]) {
                    for (const session of [owned.directory, path.toNamespacedPath(owned.directory)]) {
                        assert.equal(report.authorizedWorkspace(worker(), taskView(), { ...owned.workspace, localRoot }, state, session), expected);
                    }
                }
            }
            try { fs.realpathSync(path.toNamespacedPath(owned.directory)); }
            catch (error) { t.diagnostic(`Legacy JS realpath reproduction: ${error.code}, ${error.syscall}, ${error.path}`); }
            assert.throws(() => report.authorizedWorkspace(worker(), taskView(), owned.workspace, owned.state,
                path.toNamespacedPath(owned.root)), /exact fixture-managed/);
            assert.deepEqual(fs.readdirSync(owned.directory), []);
        } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
    });
}

test('resolves an existing same-location short-name alias back to the exact long workspace', { skip: process.platform !== 'win32' }, t => {
    const owned = ownedDirectories(true);
    try {
        const shortState = shortDirectory(owned.state), shortWorkspace = shortDirectory(owned.directory);
        if (!shortWorkspace.includes('~')) { t.skip('The owned volume did not allocate an 8.3 spelling.'); return; }
        assert.equal(fs.statSync(shortWorkspace, { bigint: true }).ino, fs.statSync(owned.directory, { bigint: true }).ino);
        assert.equal(report.authorizedWorkspace(worker(), taskView(), { ...owned.workspace, localRoot: shortWorkspace },
            shortState, shortWorkspace), fs.realpathSync.native(owned.directory));
        assert.throws(() => report.authorizedWorkspace(worker(), taskView(), owned.workspace, owned.state, shortState), /exact fixture-managed/);
        assert.deepEqual(fs.readdirSync(owned.directory), []);
    } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
});

test('rejects relative paths and directory-shaped file names before authorizing any write', () => {
    const owned = ownedDirectories();
    try {
        assert.throws(() => report.authorizedWorkspace(worker(), taskView(), owned.workspace, '.', owned.directory), /absolute existing directory/);
        assert.throws(() => report.authorizedWorkspace(worker(), taskView(), owned.workspace, owned.state, 'C:'), /absolute existing directory/);
        const file = path.join(owned.root, 'not-a-directory');
        fs.writeFileSync(file, 'owned negative control', { flag: 'wx' });
        assert.throws(() => report.authorizedWorkspace(worker(), taskView(), { ...owned.workspace, localRoot: file },
            owned.state, file), /not a directory/);
        assert.deepEqual(fs.readdirSync(owned.directory), []);
    } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
});

test('retains the first adapter error and later stacks with the exact session path, without contacting a service', () => {
    const owned = ownedDirectories(true);
    try {
        fs.writeFileSync(path.join(owned.root, 'runtime.json'), JSON.stringify({ scenario: 'WorkFlow', stateRoot: path.toNamespacedPath(owned.state) }));
        fs.writeFileSync(path.join(owned.root, 'report-work.json'), '{}');
        const sessionDirectory = path.toNamespacedPath(owned.directory);
        const input = [
            { jsonrpc: '2.0', id: 1, method: 'session/new', params: { cwd: sessionDirectory, mcpServers: [{
                name: 'agent-center-work', type: 'http', url: 'http://127.0.0.1:1/mcp',
                headers: [{ name: 'Authorization', value: 'offline-not-a-real-binding' }]
            }] } },
            { jsonrpc: '2.0', id: 2, method: 'offline-first-error' },
            { jsonrpc: '2.0', id: 3, method: 'offline-later-error' }
        ].map(value => JSON.stringify(value)).join('\n') + '\n';
        const adapter = path.join(__dirname, '..', 'fixtures', 'AgentCenterIntakeCoordinator.cjs');
        const output = execFileSync(process.execPath, [adapter, path.toNamespacedPath(owned.root), 'report'],
            { input, encoding: 'utf8', timeout: 10000, windowsHide: true });
        assert.equal(output.trim().split('\n').length, 3);
        const first = JSON.parse(fs.readFileSync(path.join(owned.root, 'report-fixture-error.json'), 'utf8'));
        const historyText = fs.readFileSync(path.join(owned.root, 'report-fixture-errors.jsonl'), 'utf8');
        const history = historyText.trim().split('\n').map(line => JSON.parse(line));
        assert.match(first.error, /offline-first-error/);
        assert.equal(history.length, 2);
        assert.match(history[1].error, /offline-later-error/);
        assert.equal(first.sessionDirectory, sessionDirectory);
        assert.ok(history.every(failure => failure.stack.includes('AgentCenterIntakeCoordinator.cjs')));
        assert.ok(!historyText.includes('offline-not-a-real-binding'));
        assert.deepEqual(fs.readdirSync(owned.directory), []);
    } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
});

test('rejects missing roots, stale reservations and unrelated session directories without a cwd fallback', () => {
    const owned = ownedDirectories();
    try {
        const check = (task, workspace, session) => report.authorizedWorkspace(worker(), task, workspace, owned.state, session);
        assert.equal(check(taskView(), owned.workspace, owned.directory), fs.realpathSync(owned.directory));
        assert.throws(() => check(taskView(), { ...owned.workspace, localRoot: '' }, owned.directory), /authoritative workspace root/);
        assert.throws(() => check(taskView(), { ...owned.workspace, writer: 'Human' }, owned.directory), /reservation/);
        assert.throws(() => check(taskView(), owned.workspace, owned.root), /exact fixture-managed/);
        const stale = taskView(); stale.attempt.acknowledged = false;
        assert.throws(() => check(stale, owned.workspace, owned.directory), /reservation/);
        assert.deepEqual(fs.readdirSync(owned.directory), []);
    } finally { fs.rmSync(owned.root, { recursive: true, force: true }); }
});
