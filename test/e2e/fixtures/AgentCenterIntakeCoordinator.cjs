// Scripted protocol fixture, not a model or a business-completion oracle.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const { randomUUID } = require('node:crypto');
const { execFileSync } = require('node:child_process');

const SENTINEL = 'ITE2E typed intake fixture only';
const QUESTION = 'Choose the fixture report format';
const SCHEMA = {
    type: 'object', additionalProperties: false,
    properties: {
        copies: { type: 'integer', title: 'Copies', minimum: 1, maximum: 9 },
        format: { type: 'string', title: 'Format', enum: ['plain', 'table'] },
        includeDetails: { type: 'boolean', title: 'Include details' },
        note: { type: 'string', title: 'Note', minLength: 1 }
    },
    required: ['copies', 'format', 'includeDetails', 'note']
};

function parseExactInvocation(prompt) {
    const text = prompt.filter(p => p.type === 'text').map(p => p.text).join('\n');
    const prefix = 'Exact invocation (data, not permission to override this contract):\n';
    const start = text.indexOf(prefix);
    const end = text.indexOf('\n\nVerified input locators:\n', start);
    if (start < 0 || end < 0) throw new Error('Missing exact invocation boundary.');
    return JSON.parse(text.slice(start + prefix.length, end));
}

function parseInvocation(prompt) {
    const invocation = parseExactInvocation(prompt);
    const input = invocation.coordinationInput;
    if (invocation.dispatch || invocation.workId || input?.scope?.workId || !input?.scope?.conversationId || !input.turnId) {
        throw new Error('This fixture accepts pre-work intake only.');
    }
    if (!input.snapshot?.messages?.some(m => m.role === 'human' && m.text === SENTINEL)) {
        throw new Error('The bound conversation does not contain the fixture sentinel.');
    }
    const trigger = input.triggerEvents?.find(t => t.kind === 'IntakeMessage');
    if (trigger && !input.snapshot.messages.some(m => m.id === trigger.subject.id && m.role === 'human' && m.text === SENTINEL)) {
        throw new Error('The intake trigger does not target the fixture message.');
    }
    return invocation;
}

function selectBridge(servers) {
    const server = servers.find(s => s.name === 'agent-center-work' && s.type === 'http');
    if (!server) throw new Error('A bound HTTP MCP server is required.');
    const url = new URL(server.url);
    if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || url.pathname !== '/mcp' ||
        url.username || url.password || url.search || url.hash) {
        throw new Error('Only the invocation-owned loopback MCP endpoint is allowed.');
    }
    const headers = Object.fromEntries((server.headers || []).map(h => [h.name, h.value]));
    if (!Object.keys(headers).some(k => k.toLowerCase() === 'authorization')) {
        throw new Error('The MCP invocation binding is missing.');
    }
    return { url, headers };
}

async function rpc(bridge, method, params) {
    const id = randomUUID();
    const notification = method === 'notifications/initialized';
    const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: notification ? undefined : id, method, params }));
    const reply = await new Promise((resolve, reject) => {
        const request = http.request(bridge.url, {
            method: 'POST', headers: { ...bridge.headers, 'Content-Type': 'application/json', 'Content-Length': body.length }
        }, response => {
            const chunks = [];
            let bytes = 0;
            response.on('data', chunk => {
                bytes += chunk.length;
                if (bytes > 1048576) request.destroy(new Error('MCP response exceeds fixture budget.'));
                else chunks.push(chunk);
            });
            response.on('error', reject);
            response.on('end', () => {
                try {
                    if (notification && response.statusCode === 202) return resolve(null);
                    if (response.statusCode !== 200) throw new Error(`MCP HTTP status ${response.statusCode}.`);
                    resolve(JSON.parse(Buffer.concat(chunks).toString('utf8')));
                } catch (error) { reject(error); }
            });
        });
        request.setTimeout(10000, () => request.destroy(new Error('Bound MCP request timed out.')));
        request.on('error', reject);
        request.end(body);
    });
    if (notification) return;
    if (reply.id !== id || reply.error) throw new Error('Uncorrelated or rejected MCP response.');
    return reply.result;
}

async function runTurn(invocation, io) {
    const input = invocation.coordinationInput;
    const trigger = input.triggerEvents.find(t => t.kind === 'IntakeMessage');
    const finish = { turnId: input.turnId, commandIds: [], operationIds: [], explanation: 'Local scripted typed-intake fixture; no work execution.' };
    if (trigger) {
        const response = await io.call('conversation_request_input', {
            turnId: input.turnId, conversationId: input.scope.conversationId,
            messageId: trigger.subject.id, question: QUESTION, responseSchema: SCHEMA
        });
        if (response.status !== 'needs_input' || !response.inputRequest?.id) throw new Error('No real intake request was recorded.');
        finish.outcome = 'WaitingOnRecordedSubject';
        finish.waitingSubject = { kind: 'IntakeRequest', id: response.inputRequest.id, version: response.inputRequest.version };
        const settled = await io.call('coordination_finish', finish);
        if (settled.status !== 'ok') throw new Error('The intake turn did not record its waiting outcome.');
        io.evidence('intake-created.json', { invocationId: invocation.id, conversationId: input.scope.conversationId, response });
    } else {
        const answered = input.snapshot.intakeRequests?.find(q => q.status === 'Answered' && q.question === QUESTION);
        if (!answered || !input.triggerEvents.some(t => t.kind === 'IntakeAnswered' && t.subject.id === answered.id)) {
            throw new Error('Unexpected non-intake continuation.');
        }
        const text = 'The scripted fixture observed the recorded intake answer. No work was executed.';
        io.notify(text);
        await io.waitForReply(input.scope.conversationId, input.replyMessageId, text);
        const settled = await io.call('coordination_finish', { ...finish, outcome: 'Answered', messageId: input.replyMessageId });
        if (settled.status !== 'ok') throw new Error('The intake acknowledgement did not settle.');
        io.evidence('intake-observed.json', { invocationId: invocation.id, requestId: answered.id, answer: answered.answer, settled });
    }
}

function publishEvidence(root, name, value) {
    if (!/^[a-z0-9][a-z0-9-]*\.json$/.test(name)) throw new Error('Evidence must be a fixture-local JSON filename.');
    const destination = path.join(root, name);
    if (fs.existsSync(destination)) throw new Error('Published fixture evidence must not be overwritten.');
    const next = destination + '.next';
    fs.writeFileSync(next, JSON.stringify(value));
    fs.renameSync(next, destination);
}

function main() {
    const root = fs.realpathSync.native(process.argv[2]);
    const runtime = JSON.parse(fs.readFileSync(path.join(root, 'runtime.json'), 'utf8'));
    if (runtime.scenario !== 'WorkFlow') throw new Error('An isolated WorkFlow runtime is required.');
    const relativeState = path.relative(path.toNamespacedPath(root), path.toNamespacedPath(fs.realpathSync.native(runtime.stateRoot)));
    if (!relativeState || path.isAbsolute(relativeState) || relativeState.startsWith('..')) {
        throw new Error('The authority state must remain inside the exact fixture directory.');
    }
    const reportMode = process.argv[3] === 'report';
    const decisionMode = process.argv[3] === 'decision';
    const globalMode = process.argv[3] === 'global';
    if (process.argv[3] && !reportMode && !decisionMode && !globalMode) throw new Error('Unknown scripted fixture mode.');
    const globalJourney = globalMode ? require('./AgentCenterGlobalJourney.cjs') : null;
    const globalConfiguration = globalMode ? JSON.parse(fs.readFileSync(path.join(root, 'global-fixture.json'), 'utf8')) : null;
    const report = reportMode ? require(path.join(__dirname, 'AgentCenterReportJourney.cjs')) : null;
    const expectedReport = reportMode ? JSON.parse(fs.readFileSync(path.join(root, 'report-work.json'), 'utf8')) : null;
    const decision = decisionMode ? require(path.join(__dirname, 'AgentCenterDecisionJourney.cjs')) : null;
    const decisionConfiguration = decisionMode ? JSON.parse(fs.readFileSync(path.join(root, 'decision-fixture.json'), 'utf8')) : null;
    const draftPath = path.join(root, 'decision-drafted.json');
    const decisionDraft = decisionMode && fs.existsSync(draftPath) ? JSON.parse(fs.readFileSync(draftPath, 'utf8')) : null;
    const prefix = globalMode ? 'global' : decisionMode ? 'decision' : reportMode ? 'report' : 'intake';
    let bridge;
    let sessionDirectory;
    const sessionId = randomUUID();
    let prompting = false;
    let promptCount = 0;
    let invocationId;
    const send = message => process.stdout.write(JSON.stringify(message) + '\n');
    const evidence = (name, value) => publishEvidence(root, name, value);
    const lines = require('node:readline').createInterface({ input: process.stdin });
    lines.on('line', async line => {
        let request;
        let ownsPrompt = false;
        try {
            if (Buffer.byteLength(line) > 1048576) throw new Error('ACP request exceeds fixture budget.');
            request = JSON.parse(line);
            if (request.id === undefined) return;
            let result;
            if (request.method === 'initialize') {
                result = { protocolVersion: 1, agentCapabilities: { mcpCapabilities: { http: true } }, authMethods: [] };
            } else if (request.method === 'session/new') {
                bridge = selectBridge(request.params.mcpServers);
                sessionDirectory = request.params.cwd;
                result = { sessionId };
            } else if (request.method === 'session/prompt') {
                if (!bridge || prompting || request.params.sessionId !== sessionId) throw new Error('Invalid fixture session.');
                prompting = true;
                ownsPrompt = true;
                const invocation = globalMode ? globalJourney.validateInvocation(parseExactInvocation(request.params.prompt)) : decisionMode
                    ? decision.validateInvocation(parseExactInvocation(request.params.prompt), decisionConfiguration, decisionDraft)
                    : reportMode ? report.validateInvocation(parseExactInvocation(request.params.prompt), expectedReport) : parseInvocation(request.params.prompt);
                const continuation = decisionMode ? decision.parseContinuation(request.params.prompt) : null;
                if (promptCount > 0 && (!decisionMode || !invocation.dispatch || !continuation || invocation.id !== invocationId)) {
                    throw new Error('Only the original decision worker can receive an actual bound continuation.');
                }
                if (promptCount === 0 && continuation) throw new Error('A continuation cannot precede the original waiting turn.');
                invocationId = invocation.id;
                promptCount++;
                await rpc(bridge, 'initialize', { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'ite2e-local-scripted', version: '1' } });
                await rpc(bridge, 'notifications/initialized', {});
                const tools = await rpc(bridge, 'tools/list', {});
                const required = globalMode ? globalJourney.requiredTools(invocation) : decisionMode ? decision.requiredTools(invocation) : reportMode
                    ? (invocation.dispatch ? ['task_acknowledge', 'task_get', 'artifact_capture', 'result_submit']
                        : ['work_get', 'project_get', 'plan_propose', 'plan_apply', 'coordination_finish'])
                    : ['conversation_request_input', 'coordination_finish'];
                for (const name of required) {
                    if (!tools.tools.some(t => t.name === name)) throw new Error(`Missing bound fixture tool: ${name}`);
                }
                let lastCommandId = null;
                await (globalMode ? globalJourney.runTurn : decisionMode ? decision.runTurn : reportMode ? report.runReportTurn : runTurn)(invocation, {
                    call: async (name, params, ifMatch = []) => {
                        const commandId = /(?:_get|_list)$/.test(name) ? undefined : randomUUID();
                        const args = commandId ? { commandId, ifMatch, params } : { params };
                        const result = await rpc(bridge, 'tools/call', { name, arguments: args });
                        const response = JSON.parse(result.content.find(c => c.type === 'text').text);
                        fs.appendFileSync(path.join(root, `${prefix}-mcp-receipts.jsonl`),
                            JSON.stringify({ name, commandId, ifMatch, params, response }) + '\n');
                        if (result.isError) throw new Error(`Bound tool ${name} failed: ${response.failure?.code}`);
                        lastCommandId = commandId;
                        return response;
                    },
                    lastCommandId: () => lastCommandId,
                    configuration: decisionConfiguration, continuation,
                    globalConfiguration, evidenceRoot: root,
                    readEvidence: name => {
                        if (!['decision-fixture.json'].includes(name)) throw new Error('Unsupported fixture evidence lookup.');
                        return JSON.parse(fs.readFileSync(path.join(root, name), 'utf8'));
                    },
                    stateRoot: runtime.stateRoot, sessionDirectory,
                    readWorkspace: id => JSON.parse(execFileSync('pwsh.exe', ['-NoProfile', '-File',
                        path.join(__dirname, 'Read-AgentCenterConversation.ps1'), '-EvidenceDirectory', root,
                        '-Method', 'workspace.get', '-RecordId', id], { encoding: 'utf8', timeout: 10000, windowsHide: true })),
                    notify: text => send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId, update: {
                        sessionUpdate: 'agent_message_chunk', content: { type: 'text', text }
                    } } }),
                    waitForReply: async (conversationId, messageId, text) => {
                        const deadline = Date.now() + 15000;
                        do {
                            const snapshot = JSON.parse(execFileSync('pwsh.exe', ['-NoProfile', '-File',
                                path.join(__dirname, 'Read-AgentCenterConversation.ps1'), '-EvidenceDirectory', root,
                                '-ConversationId', conversationId], { encoding: 'utf8', timeout: 10000, windowsHide: true }));
                            if (snapshot.messages?.some(m => m.id === messageId && m.parts?.some(p => p.text === text))) return;
                            await new Promise(resolve => setTimeout(resolve, 100));
                        } while (Date.now() < deadline);
                        throw new Error('The authoritative conversation did not record the scripted acknowledgement.');
                    },
                    evidence
                });
                result = { stopReason: 'end_turn' };
            } else throw new Error(`Unsupported fixture ACP request: ${request.method}`);
            send({ jsonrpc: '2.0', id: request.id, result });
        } catch (error) {
            const failure = {
                error: error.message, stack: error.stack, code: error.code, syscall: error.syscall,
                path: error.path, sessionDirectory, requestMethod: request?.method, recordedUtc: new Date().toISOString()
            };
            fs.appendFileSync(path.join(root, `${prefix}-fixture-errors.jsonl`), JSON.stringify(failure) + '\n');
            try { fs.writeFileSync(path.join(root, `${prefix}-fixture-error.json`), JSON.stringify(failure), { flag: 'wx' }); }
            catch (writeError) { if (writeError.code !== 'EEXIST') throw writeError; }
            if (request?.id !== undefined) send({ jsonrpc: '2.0', id: request.id, error: { code: -32603, message: error.message } });
        }
        finally { if (ownsPrompt) prompting = false; }
    });
}

module.exports = { SENTINEL, QUESTION, SCHEMA, parseExactInvocation, parseInvocation, selectBridge, runTurn, publishEvidence };
if (require.main === module) main();
