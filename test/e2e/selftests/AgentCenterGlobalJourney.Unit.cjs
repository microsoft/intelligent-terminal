// Offline contracts only: no UI, authority, ACP subprocess or external endpoint.
'use strict';
const assert = require('node:assert/strict');
const { test, after } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const fixture = require(path.join(__dirname, '..', 'fixtures', 'AgentCenterGlobalJourney.cjs'));
const decision = require(path.join(__dirname, '..', 'fixtures', 'AgentCenterDecisionJourney.cjs'));
const { parseExactInvocation, publishEvidence } = require(path.join(__dirname, '..', 'fixtures', 'AgentCenterIntakeCoordinator.cjs'));
const artifacts = path.resolve(__dirname, '..', 'artifacts');
fs.mkdirSync(artifacts, { recursive: true });
const owned = fs.mkdtempSync(path.join(artifacts, 'global-contract-'));
const root = path.join(owned, 'fixture'), approved = path.join(root, 'approved');
fs.mkdirSync(approved, { recursive: true });
after(() => fs.rmSync(owned, { recursive: true }));

const limits = { concurrency: 1, executionAttempts: 1, evaluationAttempts: 1,
    coordinationTurns: 24, contextRounds: 3, executionSeconds: 180, coordinationSeconds: 90 };
const configuration = { projectRoot: approved, projectName: 'Approved Reports',
    projectPrompt: `Please prepare an execution project for my report in ${approved}. Ask for approval before creating it.` };
function invocation(text = fixture.HELLO) {
    return { id: 'global-invocation', coordinationInput: {
        turnId: 'turn', scope: { conversationId: 'conversation' }, replyMessageId: 'reply',
        triggerEvents: [{ kind: 'IntakeMessage', subject: { id: 'human' } }],
        snapshot: { kind: 'Conversation', id: 'conversation', consoleSessionId: 'console', scope: 'Global', projects: [], works: [],
            policy: { capabilityId: 'ite2e-local-global', workerCapabilityId: 'ite2e-local-global', checkCapabilityId: 'native-check', limits },
            messages: [{ id: 'human', conversationId: 'conversation', role: 'human', text,
                context: { scope: 'Global', consoleSessionId: 'console' } }] }
    } };
}
function driver(overrides = {}) {
    const calls = [], evidence = [], order = [];
    let last;
    return { calls, evidence, order, io: {
        evidenceRoot: root, globalConfiguration: configuration,
        call: async (name, params, ifMatch = []) => {
            calls.push({ name, params, ifMatch }); last = `command-${calls.length}`;
            if (overrides[name]) return overrides[name](params, ifMatch);
            if (name.endsWith('_list')) return { status: 'ok', data: { items: [] } };
            if (name === 'conversation_propose_action') return { status: 'ok', data: { proposal: {
                id: 'proposal', kind: 'HumanActionProposal', version: 1, status: 'Open', summary: params.summary,
                messageId: params.messageId, conversationId: params.conversationId, preview: { approved: true },
                request: { method: params.method, params: params.params, ifMatch: params.ifMatch, commandId: 'frozen-command' }
            } } };
            if (name === 'coordination_finish') return { status: 'ok' };
            throw new Error(`Unexpected fixture call ${name}`);
        },
        lastCommandId: () => last,
        notify: text => order.push(['notify', text]),
        waitForReply: async (...args) => order.push(['recorded-reply', ...args]),
        evidence: (name, value) => evidence.push({ name, value })
    } };
}

test('requires actual global scope and latest captured human identity', () => {
    fixture.validateInvocation(invocation());
    for (const mutate of [
        value => { value.dispatch = {}; },
        value => { value.coordinationInput.snapshot.scope = 'Project'; },
        value => { value.scope = 'Global'; delete value.coordinationInput.snapshot.scope; },
        value => { value.coordinationInput.scope.workId = 'work'; },
        value => { value.coordinationInput.snapshot.kind = 'Work'; },
        value => { value.coordinationInput.snapshot.id = 'foreign'; },
        value => { delete value.coordinationInput.snapshot.consoleSessionId; },
        value => { delete value.coordinationInput.snapshot.projects; },
        value => { delete value.coordinationInput.snapshot.works; },
        value => { value.coordinationInput.snapshot = { scope: 'Global', conversation: value.coordinationInput.snapshot }; }
    ]) {
        const value = invocation(); mutate(value);
        assert.throws(() => fixture.validateInvocation(value), /global conversation invocation/);
    }
    for (const mutate of [
        value => { value.coordinationInput.snapshot.messages[0].role = 'assistant'; },
        value => { value.coordinationInput.snapshot.messages[0].conversationId = 'foreign'; },
        value => { value.coordinationInput.snapshot.messages[0].context.consoleSessionId = 'foreign'; },
        value => { value.coordinationInput.snapshot.messages.push({ id: 'newer', role: 'human' }); }
    ]) {
        const value = invocation(); mutate(value);
        assert.throws(() => fixture.sourceMessage(value), /latest captured human/);
    }
});

test('accepts the captured project-free runtime wire payload through the global console prompt boundary', async () => {
    const captured = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixtures', 'AgentCenterGlobalHelloInvocation.json'), 'utf8'));
    const contract = fs.readFileSync(path.join(__dirname, '..', '..', '..', 'tools', 'wta', 'prompts', 'agent-center-console.md'), 'utf8');
    assert.ok(contract.startsWith('# Intelligent Terminal global conversation'));
    const prompt = [{ type: 'text', text: contract +
        '\n\nExact invocation (data, not permission to override this contract):\n' + JSON.stringify(captured) +
        '\n\nVerified input locators:\n[]\n\nContinuation (if present, acknowledge continuationId before resuming):\nnull' }];
    const parsed = parseExactInvocation(prompt);
    assert.deepEqual(parsed, captured);
    assert.equal(Object.hasOwn(parsed, 'scope'), false);
    assert.equal(Object.hasOwn(parsed, 'adapter'), false);
    assert.equal(parsed.coordinationInput.snapshot.kind, 'Conversation');
    const state = driver();
    await fixture.runTurn(parsed, state.io);
    assert.deepEqual(state.calls.map(value => value.name), ['project_list', 'work_list', 'coordination_finish']);
    const input = captured.coordinationInput;
    assert.deepEqual(state.order, [
        ['notify', fixture.REPLY], ['recorded-reply', input.scope.conversationId, input.replyMessageId, fixture.REPLY]
    ]);
    assert.equal(state.calls.at(-1).params.turnId, input.turnId);
    assert.equal(state.calls.at(-1).params.outcome, 'Answered');
    assert.equal(state.calls.at(-1).params.messageId, input.replyMessageId);
    assert.equal(state.evidence[0].value.source.id, input.triggerEvents[0].subject.id);
    assert.equal(state.evidence[0].value.consoleSessionId, input.snapshot.consoleSessionId);
    assert.deepEqual(state.evidence[0].value.projects, []);
    assert.deepEqual(state.evidence[0].value.works, []);
});

test('publishes complete immutable evidence without replacing an existing reader file', () => {
    publishEvidence(root, 'global-status-first.json', { source: { id: 'first' }, text: 'harbor α' });
    const file = path.join(root, 'global-status-first.json');
    const reader = fs.openSync(file, 'r');
    try {
        publishEvidence(root, 'global-status-second.json', { source: { id: 'second' }, text: 'orchard α' });
        assert.equal(JSON.parse(fs.readFileSync(reader, 'utf8')).source.id, 'first');
        assert.equal(JSON.parse(fs.readFileSync(path.join(root, 'global-status-second.json'), 'utf8')).source.id, 'second');
        assert.equal(fs.existsSync(file + '.next'), false);
        assert.throws(() => publishEvidence(root, 'global-status-first.json', {}), /must not be overwritten/);
    } finally { fs.closeSync(reader); }
    assert.throws(() => publishEvidence(root, '../outside.json', {}), /fixture-local/);
});
test('records an assistant reply with zero projects and without creating work', async () => {
    const state = driver();
    await fixture.runTurn(invocation(), state.io);
    assert.deepEqual(state.calls.map(value => value.name), ['project_list', 'work_list', 'coordination_finish']);
    assert.deepEqual(state.calls[0].params, { limit: 100 });
    assert.deepEqual(state.order, [
        ['notify', fixture.REPLY], ['recorded-reply', 'conversation', 'reply', fixture.REPLY]
    ]);
    assert.equal(state.evidence[0].name, 'global-hello.json');
    assert.equal(state.evidence[0].value.consoleSessionId, 'console');
    assert.equal(state.calls.at(-1).params.outcome, 'Answered');
    assert.equal(state.calls.at(-1).params.messageId, 'reply');
});

test('refuses to qualify zero-project chat when a project already exists', async () => {
    const state = driver({ project_list: () => ({ status: 'ok', data: { items: [{ id: 'existing' }] } }) });
    await assert.rejects(fixture.runTurn(invocation(), state.io), /must not require or create a Project/);
    assert.equal(state.evidence.length, 0);
});

test('refuses zero-state credit for captured projects or captured and current works', async () => {
    for (const field of ['projects', 'works']) {
        const value = invocation();
        value.coordinationInput.snapshot[field].push({ id: 'existing' });
        const state = driver();
        await assert.rejects(fixture.runTurn(value, state.io), /must capture zero/);
        assert.equal(state.evidence.length, 0);
        assert.equal(state.calls.some(value => value.name === 'coordination_finish'), false);
    }
    const state = driver({ work_list: () => ({ status: 'ok', data: { items: [{ work: { id: 'existing' } }] } }) });
    await assert.rejects(fixture.runTurn(invocation(), state.io), /must not create a Work/);
    assert.equal(state.evidence.length, 0);
});

test('proposes the literal owned directory under exactly the approved global policy', async () => {
    const state = driver();
    await fixture.runTurn(invocation(configuration.projectPrompt), state.io);
    const proposal = state.calls.find(value => value.name === 'conversation_propose_action').params;
    assert.equal(proposal.method, 'project.configure');
    assert.equal(proposal.params.root, approved);
    assert.deepEqual(proposal.params.limits, limits);
    assert.equal(proposal.params.coordinatorCapabilityId, 'ite2e-local-global');
    assert.equal(proposal.params.workerCapabilityId, 'ite2e-local-global');
    assert.deepEqual(proposal.ifMatch, []);
    assert.equal(proposal.messageId, 'human');
    assert.equal(state.calls.some(value => value.name === 'project_configure'), false);
    assert.deepEqual(state.calls.at(-1).params.commandIds, ['command-3']);
    assert.equal(state.evidence[0].value.proposal.request.commandId, 'frozen-command');
});

test('rejects ungrounded or outside-authority roots without a cwd fallback', async () => {
    const outside = path.join(owned, 'outside');
    fs.mkdirSync(outside);
    for (const config of [
        { ...configuration, projectRoot: outside },
        { ...configuration, projectPrompt: 'A directory was not specified.' }
    ]) {
        config.projectPrompt = config.projectRoot === outside ? `Please use ${outside}.` : config.projectPrompt;
        const state = driver(); state.io.globalConfiguration = config;
        await assert.rejects(fixture.runTurn(invocation(config.projectPrompt), state.io), /literally|escaped/);
        assert.equal(state.calls.some(value => value.name === 'conversation_propose_action'), false);
    }
});

test('rejects capability changes and altered proposed parameters', async () => {
    const value = invocation(configuration.projectPrompt);
    value.coordinationInput.snapshot.policy.workerCapabilityId = 'unapproved';
    await assert.rejects(fixture.runTurn(value, driver().io), /approved global capabilities/);
    const state = driver({ conversation_propose_action: params => ({ status: 'ok', data: { proposal: {
        kind: 'HumanActionProposal', version: 1, status: 'Open', messageId: params.messageId, conversationId: params.conversationId, preview: {},
        request: { method: params.method, commandId: 'wrong-request', params: { ...params.params, root: owned }, ifMatch: [] }
    } } }) });
    await assert.rejects(fixture.runTurn(invocation(configuration.projectPrompt), state.io));
    assert.equal(state.evidence.length, 0);
});

test('keeps the proposed orchard target independent of the harbor context hint', async () => {
    const value = invocation(fixture.HOLD);
    value.coordinationInput.snapshot.messages[0].context.selectedWorkId = 'harbor';
    const state = driver({
        project_list: () => ({ status: 'ok', data: { items: [{ id: 'a', name: 'Harbor Reports' }, { id: 'b', name: 'Orchard Notes' }] } }),
        work_list: () => ({ status: 'ok', data: { items: [
            { work: { id: 'harbor', projectId: 'a' }, spec: { goal: 'Review the harbor checklist' } },
            { work: { id: 'orchard', projectId: 'b' }, spec: { goal: 'Review the orchard checklist' } }
        ] } }),
        work_get: params => {
            assert.equal(params.workId, 'orchard');
            return { status: 'ok', data: { work: { id: 'orchard', kind: 'Work', version: 42 } } };
        }
    });
    await fixture.runTurn(value, state.io);
    const proposed = state.calls.find(item => item.name === 'conversation_propose_action').params;
    assert.deepEqual(proposed.params, { workId: 'orchard', action: 'Hold' });
    assert.deepEqual(proposed.ifMatch, [{ kind: 'Work', id: 'orchard', version: 42 }]);
    assert.equal(state.calls.some(item => item.name === 'work_control'), false);
});

test('answers a global status question from both actual list views without mutating them', async () => {
    const views = [
        { work: { kind: 'Work', id: 'harbor', projectId: 'a', lifecycle: 'Draft' }, spec: { goal: 'Review the harbor checklist' } },
        { work: { kind: 'Work', id: 'orchard', projectId: 'b', lifecycle: 'Active' }, spec: { goal: 'Review the orchard checklist' } }
    ];
    const state = driver({
        project_list: () => ({ status: 'ok', data: { items: [{ id: 'a', name: 'Harbor Reports' }, { id: 'b', name: 'Orchard Notes' }] } }),
        work_list: () => ({ status: 'ok', data: { items: views } }),
        work_get: params => ({ status: 'ok', data: views.find(view => view.work.id === params.workId) })
    });
    await fixture.runTurn(invocation(fixture.STATUS), state.io);
    assert.deepEqual(state.calls.map(item => item.name), ['project_list', 'work_list', 'work_get', 'work_get', 'coordination_finish']);
    assert.deepEqual(state.calls.filter(item => item.name === 'work_get').map(item => item.params.workId), ['harbor', 'orchard']);
    assert.deepEqual(state.evidence[0].value.observedWorks, views);
    assert.equal(state.evidence[0].value.conversationId, 'conversation');
    assert.equal(state.evidence[0].value.reply,
        'Harbor Reports: Review the harbor checklist is Draft. Orchard Notes: Review the orchard checklist is Active.');
});

test('preserves the recorded privacy-filtered status failure without guessing hidden projects or works', async () => {
    // The d2ba1ebb native invocation could see only Approved Reports, not the
    // seeded projects whose policies omitted the global capability.
    const project = { kind: 'Project', id: 'ac55cc21-ade1-4ab4-88c9-15aeb1ca3bea',
        name: 'Approved Reports', capabilityIds: ['ite2e-local-global', 'native-check'] };
    const value = invocation(fixture.STATUS);
    value.coordinationInput.snapshot.projects = [{ id: project.id, name: project.name }];
    const state = driver({
        project_list: () => ({ status: 'ok', data: { items: [project] } }),
        work_list: () => ({ status: 'ok', data: { items: [] } })
    });
    await assert.rejects(fixture.runTurn(value, state.io), /one actual work in each requested project/);
    assert.deepEqual(state.calls.map(item => item.name), ['project_list', 'work_list']);
    assert.equal(state.evidence.length, 0);
    assert.equal(state.order.length, 0);
});

test('rejects ambiguous or non-view status list items instead of changing their shape', async () => {
    const project = { id: 'a', name: 'Harbor Reports' };
    const view = { work: { id: 'harbor', projectId: 'a', lifecycle: 'Draft' }, spec: { goal: 'Review the harbor checklist' } };
    for (const [projects, works] of [
        [[project, { ...project, id: 'duplicate' }], [view]],
        [[project], [view, { ...view, work: { ...view.work, id: 'second' } }]],
        [[project], [{ ...view.work, spec: view.spec }]],
        [[project], []]
    ]) {
        const state = driver({
            project_list: () => ({ status: 'ok', data: { items: projects } }),
            work_list: () => ({ status: 'ok', data: { items: works } })
        });
        await assert.rejects(fixture.runTurn(invocation(fixture.STATUS), state.io), /one actual work/);
        assert.equal(state.calls.some(item => item.name === 'work_get'), false);
        assert.equal(state.evidence.length, 0);
    }
});

test('rejects work_get facts that do not belong to the listed work and project', async () => {
    const view = { work: { kind: 'Work', id: 'harbor', projectId: 'a', lifecycle: 'Draft' }, spec: { goal: 'Recorded goal' } };
    for (const altered of [
        { ...view, work: { ...view.work, id: 'foreign' } },
        { ...view, work: { ...view.work, projectId: 'foreign' } },
        { ...view, work: { ...view.work, lifecycle: '' } },
        { ...view, spec: {} }
    ]) {
        const state = driver({
            project_list: () => ({ status: 'ok', data: { items: [{ id: 'a', name: 'Harbor Reports' }] } }),
            work_list: () => ({ status: 'ok', data: { items: [view] } }),
            work_get: () => ({ status: 'ok', data: altered })
        });
        await assert.rejects(fixture.runTurn(invocation(fixture.STATUS), state.io), /exact listed work and project/);
        assert.equal(state.evidence.length, 0);
        assert.equal(state.order.length, 0);
    }
});

test('acknowledges only an actual submitted proposal without automatically starting work', async () => {
    const value = invocation(configuration.projectPrompt);
    value.coordinationInput.triggerEvents = [{ kind: 'HumanActionSubmitted', subject: { id: 'proposal' } }];
    value.coordinationInput.snapshot.actionProposals = [{ id: 'proposal', status: 'Submitted', request: { commandId: 'frozen' } }];
    const state = driver();
    await fixture.runTurn(value, state.io);
    assert.deepEqual(state.calls.map(item => item.name), ['coordination_finish']);
    assert.equal(state.evidence[0].name, 'global-submission-frozen.json');
    value.coordinationInput.snapshot.actionProposals[0].status = 'Open';
    await assert.rejects(fixture.runTurn(value, driver().io), /actual submitted action/);
});

test('records a real-shaped draft and Start proposal rather than directly executing the global request', async () => {
    const prompt = `Please produce the human-approved decision report in ${approved} and ask for the report details before writing.`;
    const value = invocation(prompt);
    assert.equal(Object.hasOwn(value, 'scope'), false);
    value.coordinationInput.snapshot.projects = [{ id: 'project', root: approved }];
    value.coordinationInput.snapshot.messages[0].context.projectId = 'unrelated-hint';
    const work = { kind: 'Work', id: 'work', version: 1, projectId: 'project', lifecycle: 'Draft', currentSpecRevision: 1 };
    const state = driver({
        conversation_resolve_intents: () => ({ status: 'ok' }),
        work_create_draft: () => ({ status: 'ok', data: { workId: 'work' } }),
        work_get: () => ({ status: 'ok', data: { work } }),
        project_get: () => ({ status: 'ok', data: { policyRevision: 2 } }),
        grant_preview: () => ({ status: 'ok', data: { grantProposalId: 'grant' } })
    });
    state.io.globalConfiguration = { ...configuration, decisionPrompt: prompt };
    state.io.readEvidence = name => {
        assert.equal(name, 'decision-fixture.json');
        return { projectId: 'project', goal: decision.GOAL, root: approved, prompt };
    };
    await fixture.runTurn(value, state.io);
    const proposal = state.evidence.find(item => item.name === 'decision-start-proposal.json').value;
    assert.equal(proposal.request.method, 'work.start');
    assert.deepEqual(proposal.request.params, { workId: 'work', specRevision: 1, projectPolicyRevision: 2, grantProposalId: 'grant' });
    assert.equal(state.calls.some(item => item.name === 'work_start' || item.name === 'plan_apply'), false);
});
