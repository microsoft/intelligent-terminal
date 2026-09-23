// Offline adapter contracts only; mocked service replies never award native coverage.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { test } = require('node:test');
const fixture = require('../fixtures/AgentCenterDecisionJourney.cjs');

const configuration = { projectId: 'project-owned', goal: fixture.GOAL };
function intake() {
    return { id: 'intake-owned', coordinationInput: {
        turnId: 'turn-owned', scope: { conversationId: 'conversation-owned' },
        triggerEvents: [{ kind: 'IntakeMessage', subject: { id: 'message-owned' } }],
        snapshot: { messages: [{ id: 'message-owned', role: 'human', text: fixture.SENTINEL,
            context: { projectId: configuration.projectId, selectedWorkId: null } }] }
    } };
}
function view() {
    return { work: { kind: 'Work', id: 'work-owned', projectId: configuration.projectId, workspaceId: 'workspace-owned',
        version: 4, lifecycle: 'Draft', currentPlanRevision: 0 },
    spec: { goal: fixture.GOAL, criteria: [{ id: 'report', description: 'Report present', evidenceRule: 'artifact:report' }] } };
}
function worker() {
    return { id: 'worker-owned', dispatch: {
        id: 'dispatch-owned', kind: 'ProduceResult', workId: 'work-owned', taskId: 'task-owned', taskRevision: 1,
        attemptId: 'attempt-owned', workspaceId: 'workspace-owned', role: 'Integration', objective: fixture.GOAL,
        scope: ['report.txt'], gateDefinitions: [], inputManifestDigest: 'input-owned'
    } };
}
function continuation() {
    return { id: 'continuation-owned', requestId: 'context-owned', dispatchId: 'dispatch-owned', taskRevision: 1,
        compatibleInputManifestDigest: 'input-owned', answer: JSON.stringify(fixture.ANSWER) };
}

test('binds the initial brief and later invocation to the exact captured project and created work', () => {
    assert.equal(fixture.validateInvocation(intake(), configuration, null).id, 'intake-owned');
    const wrong = intake(); wrong.coordinationInput.snapshot.messages[0].context.projectId = 'foreign';
    assert.throws(() => fixture.validateInvocation(wrong, configuration, null), /captured Home\/project/);
    const selected = intake(); selected.coordinationInput.snapshot.messages[0].context.selectedWorkId = 'other';
    assert.throws(() => fixture.validateInvocation(selected, configuration, null), /captured Home\/project/);
    assert.equal(fixture.validateInvocation(worker(), configuration, { view: view() }).id, 'worker-owned');
    const foreign = worker(); foreign.dispatch.workId = 'foreign';
    assert.throws(() => fixture.validateInvocation(foreign, configuration, { view: view() }), /another work/);
    assert.throws(() => fixture.validateInvocation(worker(), configuration, null), /originate/);
});

test('parses only the production continuation boundary and retains typed answer bytes', () => {
    const prefix = '\n\nContinuation (if present, acknowledge continuationId before resuming):\n';
    assert.deepEqual(fixture.parseContinuation([{ type: 'text', text: 'contract' + prefix + JSON.stringify(continuation()) }]), continuation());
    assert.equal(fixture.parseContinuation([{ type: 'text', text: prefix + 'null' }]), null);
    assert.throws(() => fixture.parseContinuation([{ type: 'text', text: 'Continuation:\n{}' }]), /boundary/);
});

test('records a brief through bound intake actions but never starts it or prepares authority on behalf of the human', async () => {
    const calls = [], evidence = [];
    await fixture.runTurn(intake(), {
        configuration, lastCommandId: () => 'command-' + calls.length,
        call: async (name, params) => {
            calls.push({ name, params });
            if (name === 'work_create_draft') return { data: { workId: 'work-owned' } };
            if (name === 'work_get') return { data: view() };
            return { status: 'ok' };
        },
        evidence: (name, value) => evidence.push({ name, value })
    });
    assert.deepEqual(calls.map(value => value.name), ['conversation_resolve_intents', 'work_create_draft', 'work_get', 'coordination_finish']);
    assert.deepEqual(calls[1].params.scope, ['report.txt']);
    assert.deepEqual(calls[1].params.sourceMessageIds, ['message-owned']);
    assert.equal(calls[1].params.projectId, configuration.projectId);
    assert.deepEqual(calls[3].params.commandIds, ['command-1', 'command-2']);
    assert.equal(evidence[0].name, 'decision-drafted.json');
    assert.equal(evidence[0].value.view.work.lifecycle, 'Draft');
});

test('creates a TaskInput decision only for the actual blocking context and freezes its Work guard', async () => {
    const calls = [], evidence = [], current = view();
    current.plan = { id: 'plan-owned' }; current.taskSummaries = [{ id: 'task-owned' }];
    const input = { coordinationInput: { turnId: 'turn-owned', scope: { workId: 'work-owned' },
        triggerEvents: [{ kind: 'ContextRequested', subject: { id: 'context-owned' } }] } };
    const context = { id: 'context-owned', kind: 'ContextRequest', version: 2, status: 'Open',
        workId: 'work-owned', blocking: true, question: fixture.CONTEXT_QUESTION };
    const io = {
        call: async (name, params, ifMatch) => {
            calls.push({ name, params, ifMatch });
            if (name === 'work_get') return { data: current };
            if (name === 'task_get') return { data: { task: { id: 'task-owned' }, contextRequests: [context] } };
            if (name === 'decision_request') return { status: 'needs_input', inputRequest: { kind: 'Decision', id: 'decision-owned', version: 1 } };
            return { status: 'ok' };
        },
        evidence: (name, value) => evidence.push({ name, value })
    };
    await fixture.runTurn(input, io);
    assert.equal(calls[2].params.purpose, 'TaskInput');
    assert.equal(calls[2].params.contextRequestId, context.id);
    assert.deepEqual(calls[2].ifMatch, [{ kind: 'Work', id: 'work-owned', version: 4 }]);
    assert.deepEqual(calls[2].params.responseSchema, fixture.SCHEMA);
    assert.deepEqual(calls[3].params.waitingSubject, { kind: 'DecisionRequest', id: 'decision-owned', version: 1 });
    assert.equal(evidence[0].name, 'decision-created.json');
    context.status = 'Answered'; calls.length = 0;
    await assert.rejects(fixture.runTurn(input, io), /actual blocking context/);
    assert.deepEqual(calls.map(value => value.name), ['work_get', 'task_get']);
});

test('the first worker turn acknowledges then requests context without writing or fabricating a result', async () => {
    const calls = [], evidence = [];
    await fixture.runTurn(worker(), {
        call: async (name, params) => {
            calls.push({ name, params });
            return name === 'task_request_context'
                ? { status: 'needs_input', inputRequest: { kind: 'Context', id: 'context-owned' } }
                : { status: 'ok' };
        },
        evidence: (name, value) => evidence.push({ name, value })
    });
    assert.deepEqual(calls.map(value => value.name), ['task_acknowledge', 'task_request_context']);
    assert.equal(calls[1].params.blocking, true);
    assert.deepEqual(calls[1].params.target, { kind: 'Coordinator' });
    assert.equal(evidence[0].name, 'decision-context-requested.json');
});

test('applies the original typed continuation before writing and capturing bytes in the acknowledged workspace', async () => {
    const root = path.join(__dirname, '..', 'artifacts', 'decision-unit-' + randomUUID());
    const state = path.join(root, 'state'), directory = path.join(state, 'workspaces', 'workspace-owned');
    fs.mkdirSync(directory, { recursive: true });
    let acknowledged = false;
    const calls = [], evidence = [];
    const io = {
        continuation: continuation(), stateRoot: state, sessionDirectory: path.toNamespacedPath(directory),
        readWorkspace: () => ({ id: 'workspace-owned', workId: 'work-owned', status: 'Ready', writer: 'None', localRoot: directory }),
        call: async (name, params) => {
            calls.push({ name, params });
            if (name === 'task_acknowledge') { assert.equal(params.continuationId, 'continuation-owned'); acknowledged = true; }
            if (name === 'task_get') return { data: {
                task: { id: 'task-owned', revision: 1 },
                attempt: { id: 'attempt-owned', invocationId: 'worker-owned', dispatchId: 'dispatch-owned', waitingRequestId: 'context-owned',
                    mode: 'ExclusiveWrite', reservationHeld: true, acknowledged },
                contextRequests: [{ id: 'context-owned', status: acknowledged ? 'Applied' : 'Answered', continuationId: 'continuation-owned' }]
            } };
            if (name === 'artifact_capture') {
                assert.equal(fs.readFileSync(path.join(directory, 'report.txt'), 'utf8'), `${fixture.MARKER}\n${JSON.stringify(fixture.ANSWER)}\n`);
                return { data: { artifacts: [{ artifactId: 'artifact-owned', digest: 'offline-digest' }] } };
            }
            return { status: 'ok' };
        },
        evidence: (name, value) => evidence.push({ name, value })
    };
    try {
        const foreign = { ...io, continuation: { ...continuation(), dispatchId: 'foreign' } };
        await assert.rejects(fixture.runTurn(worker(), foreign), /original report dispatch/);
        assert.equal(calls.length, 0);
        const untyped = { ...io, continuation: { ...continuation(), answer: JSON.stringify({ ...fixture.ANSWER, copies: '2' }) } };
        await assert.rejects(fixture.runTurn(worker(), untyped), /exact typed native decision/);
        assert.deepEqual(fs.readdirSync(directory), []);
        calls.length = 0;
        await fixture.runTurn(worker(), io);
        assert.deepEqual(calls.map(value => value.name), ['task_get', 'task_acknowledge', 'task_get', 'task_get', 'artifact_capture', 'result_submit']);
        assert.equal(evidence[0].value.continuation.id, 'continuation-owned');
        assert.equal(evidence[0].value.applied.contextRequests[0].status, 'Applied');
        assert.equal(evidence[1].name, 'decision-report-submitted.json');
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
