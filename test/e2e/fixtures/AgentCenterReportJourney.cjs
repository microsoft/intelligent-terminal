// One real artifact-presence Report journey. Never a claim of model autonomy.
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const GOAL = 'Produce the fixture report';
const MARKER = 'ITE2E CAPTURED REPORT 2026';
const CONTENT = `${MARKER}\nThis fixed report proves artifact presence only.\n`;
const reference = record => ({ kind: record.kind, id: record.id, version: record.version });
function canonical(directory) {
    if (typeof directory !== 'string' || !path.isAbsolute(directory)) {
        throw new Error('An absolute existing directory is required; no process cwd fallback is permitted.');
    }
    // The JS walker can lstat "C:" for a verbatim ACP cwd. The native resolver
    // resolves verbatim and 8.3 spellings to the same actual directory.
    const resolved = fs.realpathSync.native(directory);
    if (!fs.statSync(resolved).isDirectory()) throw new Error('The authoritative path is not a directory.');
    if (resolved.startsWith('\\\\?\\UNC\\')) return '\\\\' + resolved.slice(8);
    return /^\\\\\?\\[a-z]:\\/i.test(resolved) ? resolved.slice(4) : resolved;
}

function validateInvocation(invocation, expected) {
    const dispatch = invocation.dispatch;
    const workId = dispatch?.workId || invocation.coordinationInput?.scope?.workId;
    if (workId !== expected.id) {
        throw new Error('Report invocation is outside the explicitly started fixture work.');
    }
    if (dispatch) {
        if (dispatch.kind !== 'ProduceResult' || dispatch.objective !== GOAL ||
            dispatch.workspaceId !== expected.workspaceId || dispatch.role !== 'Integration' ||
            dispatch.scope?.length !== 1 || dispatch.scope[0] !== 'report.txt' ||
            dispatch.gateDefinitions?.length !== 0) {
            throw new Error('The report dispatch does not match the single approved artifact-presence contract.');
        }
    } else if (invocation.coordinationInput?.snapshot?.spec?.goal !== GOAL ||
        invocation.coordinationInput.snapshot.work?.projectId !== expected.projectId) {
        throw new Error('The report coordinator received a different approved goal.');
    }
    return invocation;
}

function makePlan(view, project, expectedGoal = GOAL) {
    if (view.spec.goal !== expectedGoal || view.spec.criteria?.length !== 1 ||
        view.spec.criteria[0].id !== 'report' || view.spec.criteria[0].evidenceRule !== 'artifact:report') {
        throw new Error('The report plan cannot weaken or replace the approved presence criterion.');
    }
    return {
        workId: view.work.id, basedOnPlanRevision: view.work.currentPlanRevision,
        tasks: [{
            clientKey: 'report', role: 'Integration', objective: expectedGoal, scope: ['report.txt'], exclusions: [], inputSlots: [],
            outputs: [{ slot: 'report', kind: 'Report', required: true }],
            criteria: [{ id: 'report', description: view.spec.criteria[0].description,
                evidenceRule: 'artifact:report', requiredEvidence: ['artifact:report'] }],
            gateDefinitions: [], reviewPolicy: { revision: 1, required: false, rule: 'AllRequiredGatesThenReview' },
            capabilityId: project.workerCapabilityId, requiredForDelivery: true,
            resourceRequirements: { workspaceId: view.work.workspaceId, mode: 'ExclusiveWrite' }
        }],
        edges: [], integrationTaskKey: 'report', reason: 'Produce one captured report; no executable or OS check is claimed'
    };
}

function authorizedWorkspace(invocation, taskView, workspace, stateRoot, sessionDirectory) {
    const dispatch = invocation.dispatch, attempt = taskView.attempt;
    if (workspace.id !== dispatch.workspaceId || workspace.workId !== dispatch.workId || workspace.status !== 'Ready' ||
        workspace.writer !== 'None' || taskView.task?.id !== dispatch.taskId ||
        taskView.task?.revision !== dispatch.taskRevision || attempt?.id !== dispatch.attemptId ||
        attempt.invocationId !== invocation.id || attempt.dispatchId !== dispatch.id ||
        attempt.mode !== 'ExclusiveWrite' || attempt.reservationHeld !== true || attempt.acknowledged !== true) {
        throw new Error('The current dispatch does not hold the acknowledged exclusive workspace reservation.');
    }
    if (!workspace.localRoot || !sessionDirectory) throw new Error('An authoritative workspace root and ACP session directory are required.');
    const root = canonical(stateRoot), directory = canonical(workspace.localRoot);
    const relative = path.relative(root, directory);
    if (!relative || path.isAbsolute(relative) || relative.startsWith('..') ||
        path.dirname(directory).toLowerCase() !== path.join(root, 'workspaces').toLowerCase() ||
        path.basename(directory).toLowerCase() !== dispatch.workspaceId.toLowerCase() ||
        canonical(sessionDirectory).toLowerCase() !== directory.toLowerCase()) {
        throw new Error('The authorized workspace is not the exact fixture-managed workspace.');
    }
    return directory;
}

async function runReportTurn(invocation, io) {
    if (!invocation.dispatch) {
        const input = invocation.coordinationInput;
        const view = (await io.call('work_get', { workId: input.scope.workId })).data;
        if (view.plan) {
            if (!input.triggerEvents.every(t => t.kind === 'WorkStarted')) throw new Error('Unexpected report replanning trigger.');
            await io.call('coordination_finish', {
                turnId: input.turnId, outcome: 'NoActionNeeded', commandIds: [], operationIds: [],
                explanation: 'The sole WorkStarted trigger already has its admitted report plan.'
            });
            return;
        }
        const project = (await io.call('project_get', { projectId: view.work.projectId })).data;
        const proposed = await io.call('plan_propose', makePlan(view, project), [reference(view.work)]);
        const fresh = (await io.call('work_get', { workId: view.work.id })).data;
        const applied = await io.call('plan_apply', { proposalId: proposed.data.proposalId }, [
            reference(fresh.work), { kind: 'PlanProposal', id: proposed.data.proposalId, version: proposed.data.version }
        ]);
        const appliedCommand = io.lastCommandId();
        await io.call('coordination_finish', {
            turnId: input.turnId, outcome: 'ActionsRecorded', commandIds: [appliedCommand], operationIds: [],
            explanation: 'The actual single-task report plan was admitted.'
        });
        io.evidence('report-planned.json', { proposed, applied, appliedCommand });
        return;
    }
    const dispatch = invocation.dispatch;
    await io.call('task_acknowledge', { dispatchId: dispatch.id, taskRevision: dispatch.taskRevision, disposition: 'Accepted' });
    await captureReport(invocation, io);
}

async function captureReport(invocation, io, content = CONTENT) {
    const dispatch = invocation.dispatch;
    const task = (await io.call('task_get', { taskId: dispatch.taskId })).data;
    const observed = io.readWorkspace(dispatch.workspaceId);
    const workspace = observed.workspace || observed;
    const directory = authorizedWorkspace(invocation, task, workspace, io.stateRoot, io.sessionDirectory);
    fs.writeFileSync(path.join(directory, 'report.txt'), content, { encoding: 'utf8', flag: 'wx' });
    const captured = await io.call('artifact_capture', {
        workspaceId: dispatch.workspaceId, sources: [{ kind: 'File', relativePath: 'report.txt' }], purpose: 'Output'
    });
    if (captured.data?.artifacts?.length !== 1 || !captured.data.artifacts[0].digest) throw new Error('The service did not capture exactly one report artifact.');
    const artifact = captured.data.artifacts[0];
    const submitted = await io.call('result_submit', {
        dispatchId: dispatch.id, taskRevision: dispatch.taskRevision, inputManifestDigest: dispatch.inputManifestDigest,
        outputs: [{ slot: 'report', artifact }],
        criterionEvidence: [{ criterionId: 'report', evidence: [artifact], claim: 'The required immutable report artifact is present.' }],
        summary: 'One fixed report was captured from the authorized workspace.', knownGaps: []
    });
    io.evidence('report-submitted.json', { dispatch, workspace: { id: workspace.id, localRoot: directory }, captured, submitted });
}

module.exports = { GOAL, MARKER, CONTENT, validateInvocation, makePlan, authorizedWorkspace, runReportTurn, captureReport };
