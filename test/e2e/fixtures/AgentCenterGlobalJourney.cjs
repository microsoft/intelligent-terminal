// Exact scripted conversations cross real ACP/MCP; they do not interpret language.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const intake = require('./AgentCenterIntakeCoordinator.cjs');
const decision = require('./AgentCenterDecisionJourney.cjs');

const HELLO = 'Hello.';
const REPLY = 'Hello. We can talk here without configuring a project. Nothing was started.';
const HOLD = 'Please hold Review the orchard checklist in Orchard Notes. Ask me before changing it.';
const STATUS = 'How are the harbor and orchard checklists progressing?';
const PROJECT_SUMMARY = 'Approve the report execution project';
const HOLD_SUMMARY = 'Hold the orchard checklist';

function validateInvocation(invocation) {
    const input = invocation.coordinationInput;
    const snapshot = input?.snapshot;
    if (invocation.dispatch || input?.scope?.workId ||
        !input?.turnId || !input.scope?.conversationId || snapshot?.scope !== 'Global' ||
        snapshot.kind !== 'Conversation' || snapshot.id !== input.scope.conversationId ||
        typeof snapshot.consoleSessionId !== 'string' || !snapshot.consoleSessionId ||
        !Array.isArray(snapshot.messages) || !Array.isArray(snapshot.projects) ||
        !Array.isArray(snapshot.works) || !Array.isArray(input.triggerEvents) || !input.replyMessageId) {
        throw new Error('Expected an actual global conversation invocation and captured summaries.');
    }
    return invocation;
}

function items(response) {
    if (response.status !== 'ok' || !Array.isArray(response.data?.items)) {
        throw new Error('The real list response did not contain items.');
    }
    return response.data.items;
}

function sourceMessage(invocation) {
    const input = invocation.coordinationInput;
    const trigger = input.triggerEvents.find(value => value.kind === 'IntakeMessage');
    const source = input.snapshot.messages.find(value => value.id === trigger?.subject?.id);
    const latest = input.snapshot.messages.filter(value => value.role === 'human').at(-1);
    if (!source || source !== latest || source.context?.scope !== 'Global' ||
        source.conversationId !== input.scope.conversationId ||
        source.context.consoleSessionId !== input.snapshot.consoleSessionId) {
        throw new Error('The fixture requires the latest captured human global message.');
    }
    return source;
}

async function finish(invocation, io, text, commands = []) {
    const input = invocation.coordinationInput;
    io.notify(text);
    await io.waitForReply(input.scope.conversationId, input.replyMessageId, text);
    await io.call('coordination_finish', {
        turnId: input.turnId, outcome: commands.length ? 'ActionsRecorded' : 'Answered',
        messageId: input.replyMessageId, commandIds: commands, operationIds: [],
        explanation: text
    });
}

async function runTurn(invocation, io) {
    validateInvocation(invocation);
    const input = invocation.coordinationInput;
    const submitted = input.triggerEvents.find(value => value.kind === 'HumanActionSubmitted');
    if (submitted) {
        const proposal = input.snapshot.actionProposals?.find(value => value.id === submitted.subject.id);
        if (proposal?.status !== 'Submitted') throw new Error('No actual submitted action matches the global trigger.');
        await finish(invocation, io, 'Your explicit approval was recorded. Execution and delivery remain separate states.');
        io.evidence(`global-submission-${proposal.request.commandId}.json`, { invocationId: invocation.id, proposal });
        return;
    }
    if (input.triggerEvents.some(value => value.kind === 'IntakeAnswered')) {
        return intake.runTurn(invocation, io);
    }
    const source = sourceMessage(invocation);
    if (source.text === intake.SENTINEL) return intake.runTurn(invocation, io);
    if (source.text === io.globalConfiguration.decisionPrompt) {
        const configuration = io.readEvidence('decision-fixture.json');
        decision.validateInvocation(invocation, configuration, null);
        return decision.runTurn(invocation, { ...io, configuration });
    }
    const projects = items(await io.call('project_list', { limit: 100 }));
    const works = items(await io.call('work_list', { limit: 100 }));
    if (source.text === HELLO) {
        assert.equal(input.snapshot.projects.length, 0, 'Hello must capture zero Projects.');
        assert.equal(input.snapshot.works.length, 0, 'Hello must capture zero Works.');
        assert.equal(projects.length, 0, 'Hello must not require or create a Project.');
        assert.equal(works.length, 0, 'Hello must not create a Work.');
        await finish(invocation, io, REPLY);
        io.evidence('global-hello.json', {
            invocationId: invocation.id, conversationId: input.scope.conversationId,
            consoleSessionId: source.context.consoleSessionId, source, replyMessageId: input.replyMessageId,
            reply: REPLY, projects, works, scope: input.snapshot.scope
        });
        return;
    }
    if (source.text === STATUS) {
        const selected = [], observedWorks = [];
        for (const name of ['Harbor Reports', 'Orchard Notes']) {
            const project = projects.filter(value => value.name === name);
            const work = works.filter(value => value.work?.projectId === project[0]?.id);
            if (project.length !== 1 || !project[0].id || work.length !== 1 || !work[0].work.id || !work[0].work.lifecycle) {
                throw new Error('Status must come from one actual work in each requested project.');
            }
            const response = await io.call('work_get', { workId: work[0].work.id });
            const view = response.data;
            if (response.status !== 'ok' || view?.work?.kind !== 'Work' ||
                view.work.id !== work[0].work.id || view.work.projectId !== project[0].id ||
                !view.work.lifecycle || !view.spec?.goal) {
                throw new Error('Status work_get must return the exact listed work and project with recorded facts.');
            }
            observedWorks.push(view);
            selected.push(`${name}: ${view.spec.goal} is ${view.work.lifecycle}.`);
        }
        const reply = selected.join(' ');
        await finish(invocation, io, reply);
        const recorded = { conversationId: input.scope.conversationId,
            source, projects, works, observedWorks, reply, replyMessageId: input.replyMessageId };
        io.evidence(`global-status-${source.id}.json`, recorded);
        return;
    }
    let method, params, guards = [], summary, evidence;
    if (source.text === io.globalConfiguration.projectPrompt) {
        const root = io.globalConfiguration.projectRoot;
        if (!path.isAbsolute(root) || !fs.statSync(root).isDirectory() || !source.text.includes(root)) {
            throw new Error('The execution directory must exist literally in the captured human message.');
        }
        const owned = path.relative(fs.realpathSync.native(io.evidenceRoot), fs.realpathSync.native(root));
        if (!owned || owned.startsWith('..') || path.isAbsolute(owned)) throw new Error('Project approval escaped the owned fixture.');
        const policy = input.snapshot.policy;
        if (policy?.capabilityId !== 'ite2e-local-global' || policy.workerCapabilityId !== policy.capabilityId ||
            policy.checkCapabilityId !== 'native-check' || !policy.limits) throw new Error('Unexpected approved global capabilities.');
        method = 'project.configure';
        params = { name: io.globalConfiguration.projectName, root,
            coordinatorCapabilityId: policy.capabilityId, workerCapabilityId: policy.workerCapabilityId,
            checkCapabilityId: policy.checkCapabilityId, limits: policy.limits };
        summary = PROJECT_SUMMARY;
        evidence = 'global-project-proposal.json';
    } else if (source.text === HOLD) {
        const project = projects.filter(value => value.name === 'Orchard Notes');
        const target = works.filter(value => value.work?.projectId === project[0]?.id && value.spec?.goal === 'Review the orchard checklist');
        if (project.length !== 1 || target.length !== 1) throw new Error('The captured request must resolve to exactly the owned orchard work.');
        const work = (await io.call('work_get', { workId: target[0].work.id })).data.work;
        method = 'work.control'; params = { workId: work.id, action: 'Hold' };
        guards = [{ kind: 'Work', id: work.id, version: work.version }];
        summary = HOLD_SUMMARY; evidence = 'global-hold-proposal.json';
    } else {
        throw new Error('Unknown literal global fixture message; no language parser or fallback is permitted.');
    }
    const response = await io.call('conversation_propose_action', {
        conversationId: input.scope.conversationId, messageId: source.id,
        method, params, ifMatch: guards, summary
    });
    const commandId = io.lastCommandId();
    const proposal = response.data?.proposal;
    if (response.status !== 'ok' || proposal?.kind !== 'HumanActionProposal' || proposal.status !== 'Open' ||
        !proposal.request?.commandId || proposal.request.method !== method ||
        proposal.conversationId !== input.scope.conversationId || proposal.messageId !== source.id ||
        !Number.isInteger(proposal.version) || proposal.version < 1 || !proposal.preview) {
        throw new Error('No actual open proposal with authoritative preview was recorded.');
    }
    assert.deepEqual(proposal.request.params, params);
    assert.deepEqual(proposal.request.ifMatch, guards);
    await finish(invocation, io, summary + '. Please review the captured approval before anything changes.', [commandId]);
    io.evidence(evidence, { invocationId: invocation.id, conversationId: input.scope.conversationId,
        source, projects, works, proposal });
}

const requiredTools = () => ['project_list', 'work_list', 'work_get', 'project_get', 'grant_preview',
    'work_create_draft', 'conversation_resolve_intents', 'conversation_request_input',
    'conversation_propose_action', 'coordination_finish'];
module.exports = { HELLO, REPLY, HOLD, STATUS, PROJECT_SUMMARY, HOLD_SUMMARY, validateInvocation, sourceMessage, runTurn, requiredTools };
