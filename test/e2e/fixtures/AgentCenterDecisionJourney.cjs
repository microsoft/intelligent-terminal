// Deterministic initial brief and TaskInput continuation, never model autonomy.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const report = require('./AgentCenterReportJourney.cjs');

const GOAL = 'Produce the human-approved decision report';
const SENTINEL = 'ITE2E starter report in report.txt; ask for human details.';
const QUESTION = 'Choose the approved report details';
const CONTEXT_QUESTION = 'The report needs the recorded human details before any file is written.';
const MARKER = 'ITE2E DECISION CONTINUATION REPORT';
const ANSWER = { copies: 2, format: 'detailed', approved: true, note: 'harbor α' };
const SCHEMA = {
    type: 'object', additionalProperties: false,
    properties: {
        copies: { type: 'integer', title: 'Copies', minimum: 1, maximum: 9 },
        format: { type: 'string', title: 'Format', enum: ['brief', 'detailed'] },
        approved: { type: 'boolean', title: 'Approved' },
        note: { type: 'string', title: 'Note', minLength: 1 }
    },
    required: ['copies', 'format', 'approved', 'note']
};
const reference = value => ({ kind: value.kind, id: value.id, version: value.version });

function parseContinuation(prompt) {
    const text = prompt.filter(part => part.type === 'text').map(part => part.text).join('\n');
    const marker = '\n\nContinuation (if present, acknowledge continuationId before resuming):\n';
    const index = text.indexOf(marker);
    if (index < 0) throw new Error('The actual continuation boundary is missing.');
    return JSON.parse(text.slice(index + marker.length));
}

function validateInvocation(invocation, configuration, drafted) {
    if (!configuration.projectId || configuration.goal !== GOAL) throw new Error('The approved decision fixture configuration is missing.');
    const input = invocation.coordinationInput;
    if (!invocation.dispatch && !input?.scope?.workId) {
        const trigger = input?.triggerEvents?.find(event => event.kind === 'IntakeMessage');
        const source = input?.snapshot?.messages?.find(message => message.id === trigger?.subject?.id);
        const global = input?.snapshot?.scope === 'Global';
        if (!input?.turnId || !input.scope.conversationId || source?.role !== 'human' ||
            source.text !== (global ? configuration.prompt : SENTINEL) ||
            (!global && (source.context?.projectId !== configuration.projectId || source.context?.selectedWorkId))) {
            throw new Error('The initial brief is not the exact captured Home/project message.');
        }
        if (global) {
            const project = input.snapshot.projects?.find(value => value.id === configuration.projectId);
            if (!configuration.root || !source.text.includes(configuration.root) || !project?.root ||
                fs.realpathSync.native(project.root) !== fs.realpathSync.native(configuration.root)) {
                throw new Error('The global brief must name the exact captured approved project directory.');
            }
        }
    } else {
        if (!drafted?.view?.work?.id || drafted.view.work.projectId !== configuration.projectId) {
            throw new Error('The work must originate from this fixture intake receipt.');
        }
        const dispatch = invocation.dispatch;
        const workId = dispatch?.workId || input?.scope?.workId;
        if (workId !== drafted.view.work.id) throw new Error('The decision invocation belongs to another work.');
        if (dispatch && (dispatch.kind !== 'ProduceResult' || dispatch.objective !== GOAL ||
            dispatch.workspaceId !== drafted.view.work.workspaceId || dispatch.role !== 'Integration' ||
            dispatch.scope?.length !== 1 || dispatch.scope[0] !== 'report.txt' || dispatch.gateDefinitions?.length !== 0)) {
            throw new Error('The decision dispatch is outside the exact approved report contract.');
        }
    }
    return invocation;
}

function requiredTools(invocation) {
    if (invocation.dispatch) return ['task_acknowledge', 'task_get', 'task_request_context', 'artifact_capture', 'result_submit'];
    if (!invocation.coordinationInput.scope.workId) return ['conversation_resolve_intents', 'work_create_draft', 'work_get', 'coordination_finish'];
    return ['work_get', 'project_get', 'task_get', 'plan_propose', 'plan_apply', 'decision_request', 'coordination_finish'];
}

async function runTurn(invocation, io) {
    const input = invocation.coordinationInput;
    if (!invocation.dispatch && !input.scope.workId) {
        const source = input.triggerEvents.find(event => event.kind === 'IntakeMessage').subject.id;
        const resolved = await io.call('conversation_resolve_intents', {
            turnId: input.turnId, messageId: source,
            intents: [{ kind: 'NewWork', draftGoal: GOAL, referencedMessageIds: [source] }]
        });
        const commands = [io.lastCommandId()];
        const params = {
            projectId: io.configuration.projectId, goal: GOAL, scope: ['report.txt'], exclusions: [],
            context: [], sourceMessageIds: [source],
            criteria: [{ id: 'report', description: 'The required captured report is present', evidenceRule: 'artifact:report' }],
            delivery: { kind: 'Report' }
        };
        const created = await io.call('work_create_draft', params);
        commands.push(io.lastCommandId());
        const view = (await io.call('work_get', { workId: created.data.workId })).data;
        if (view.work.lifecycle !== 'Draft' || view.work.projectId !== io.configuration.projectId) {
            throw new Error('Intake did not create the exact unstarted draft.');
        }
        if (input.snapshot.scope === 'Global') {
            const project = (await io.call('project_get', { projectId: view.work.projectId })).data;
            const guard = [reference(view.work)];
            const grant = await io.call('grant_preview', {
                workId: view.work.id, specRevision: view.work.currentSpecRevision, policyRevision: project.policyRevision
            }, guard);
            commands.push(io.lastCommandId());
            const proposed = await io.call('conversation_propose_action', {
                conversationId: input.scope.conversationId, messageId: source, method: 'work.start',
                params: { workId: view.work.id, specRevision: view.work.currentSpecRevision,
                    projectPolicyRevision: project.policyRevision, grantProposalId: grant.data.grantProposalId },
                ifMatch: guard, summary: 'Start the human-approved decision report'
            });
            commands.push(io.lastCommandId());
            if (proposed.data?.proposal?.status !== 'Open' || !proposed.data.proposal.request?.commandId) {
                throw new Error('No actual frozen Start proposal was recorded.');
            }
            io.evidence('decision-start-proposal.json', proposed.data.proposal);
        }
        await io.call('coordination_finish', {
            turnId: input.turnId, outcome: 'ActionsRecorded', commandIds: commands, operationIds: [],
            explanation: 'Recorded the initial brief only; human approval is required before starting.'
        });
        io.evidence('decision-drafted.json', { sourceMessageId: source, conversationId: input.scope.conversationId, params, resolved, created, view });
        return;
    }
    if (!invocation.dispatch) {
        const view = (await io.call('work_get', { workId: input.scope.workId })).data;
        if (!view.plan) {
            if (!input.triggerEvents.every(event => event.kind === 'WorkStarted')) throw new Error('Unexpected initial decision-work trigger.');
            const project = (await io.call('project_get', { projectId: view.work.projectId })).data;
            const proposed = await io.call('plan_propose', report.makePlan(view, project, GOAL), [reference(view.work)]);
            const fresh = (await io.call('work_get', { workId: view.work.id })).data;
            await io.call('plan_apply', { proposalId: proposed.data.proposalId }, [
                reference(fresh.work), { kind: 'PlanProposal', id: proposed.data.proposalId, version: proposed.data.version }
            ]);
            await io.call('coordination_finish', {
                turnId: input.turnId, outcome: 'ActionsRecorded', commandIds: [io.lastCommandId()], operationIds: [],
                explanation: 'Admitted one report task which must request human context before writing.'
            });
            return;
        }
        const trigger = input.triggerEvents.find(event => event.kind === 'ContextRequested');
        if (!trigger || view.taskSummaries?.length !== 1) throw new Error('The coordinator has no actual single-task context request.');
        const task = (await io.call('task_get', { taskId: view.taskSummaries[0].id })).data;
        const context = task.contextRequests.find(value => value.id === trigger.subject.id);
        if (context?.status !== 'Open' || context.blocking !== true || context.question !== CONTEXT_QUESTION ||
            context.workId !== view.work.id) throw new Error('The actual blocking context request does not match this report.');
        const requested = await io.call('decision_request', {
            workId: view.work.id, purpose: 'TaskInput', subject: reference(view.work),
            application: { requestId: context.id }, contextRequestId: context.id, question: QUESTION,
            options: [], responseSchema: SCHEMA
        }, [reference(view.work)]);
        if (requested.status !== 'needs_input' || requested.inputRequest?.kind !== 'Decision') {
            throw new Error('The bound coordinator did not record a real DecisionRequest.');
        }
        await io.call('coordination_finish', {
            turnId: input.turnId, outcome: 'WaitingOnRecordedSubject', commandIds: [], operationIds: [],
            waitingSubject: { kind: 'DecisionRequest', id: requested.inputRequest.id, version: requested.inputRequest.version },
            explanation: 'Waiting for the exact recorded human TaskInput decision.'
        });
        io.evidence('decision-created.json', { workId: view.work.id, taskId: task.task.id, context, requested });
        return;
    }
    const dispatch = invocation.dispatch;
    if (!io.continuation) {
        await io.call('task_acknowledge', { dispatchId: dispatch.id, taskRevision: dispatch.taskRevision, disposition: 'Accepted' });
        const requested = await io.call('task_request_context', {
            dispatchId: dispatch.id, taskRevision: dispatch.taskRevision, question: CONTEXT_QUESTION,
            target: { kind: 'Coordinator' }, inputs: [], blocking: true
        });
        if (requested.status !== 'needs_input' || requested.inputRequest?.kind !== 'Context' || !requested.inputRequest.id) {
            throw new Error('The worker did not enter actual context waiting.');
        }
        io.evidence('decision-context-requested.json', { invocationId: invocation.id, dispatch, requested });
        return;
    }
    const continuation = io.continuation;
    if (!continuation.id || continuation.dispatchId !== dispatch.id || continuation.taskRevision !== dispatch.taskRevision ||
        continuation.compatibleInputManifestDigest !== dispatch.inputManifestDigest) {
        throw new Error('The continuation does not belong to the original report dispatch.');
    }
    const waiting = (await io.call('task_get', { taskId: dispatch.taskId })).data;
    if (waiting.attempt?.id !== dispatch.attemptId || waiting.attempt.invocationId !== invocation.id ||
        waiting.attempt.waitingRequestId !== continuation.requestId) throw new Error('The continuation does not resume the waiting attempt.');
    const answer = JSON.parse(continuation.answer);
    assert.deepEqual(answer, ANSWER, 'The continuation must carry the exact typed native decision.');
    const acknowledged = await io.call('task_acknowledge', {
        dispatchId: dispatch.id, taskRevision: dispatch.taskRevision, disposition: 'Accepted', continuationId: continuation.id
    });
    const applied = (await io.call('task_get', { taskId: dispatch.taskId })).data;
    const context = applied.contextRequests.find(value => value.id === continuation.requestId);
    if (context?.status !== 'Applied' || context.continuationId !== continuation.id) {
        throw new Error('The real acknowledgement did not apply the original context.');
    }
    io.evidence('decision-continuation.json', { invocationId: invocation.id, dispatch, continuation, answer, acknowledged, applied });
    await report.captureReport(invocation, {
        ...io, evidence: (name, value) => io.evidence('decision-' + name, value)
    }, `${MARKER}\n${JSON.stringify(answer)}\n`);
}

module.exports = { GOAL, SENTINEL, QUESTION, CONTEXT_QUESTION, MARKER, ANSWER, SCHEMA, parseContinuation, validateInvocation, requiredTools, runTurn };
