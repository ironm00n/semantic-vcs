// A minimal ACP v1 agent over stdio, for testing the Rust client without a model.
// One prompt turn: tool_call → request_permission → tool_call_update → message → end_turn.
import readline from 'node:readline';

const rl = readline.createInterface({ input: process.stdin });
const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
let nextId = 100;
const pending = new Map();
const request = (method, params) =>
  new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    send({ jsonrpc: '2.0', id, method, params });
  });
const notify = (method, params) => send({ jsonrpc: '2.0', method, params });

process.stderr.write('fake agent up\n');

rl.on('line', async (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.id !== undefined && msg.method === undefined) {
    pending.get(msg.id)?.(msg.result ?? msg.error);
    pending.delete(msg.id);
    return;
  }
  const reply = (result) => send({ jsonrpc: '2.0', id: msg.id, result });
  switch (msg.method) {
    case 'initialize':
      reply({ protocolVersion: 1, agentCapabilities: {}, authMethods: [], agentInfo: { name: 'fake', version: '0' } });
      break;
    case 'session/new':
      if (!msg.params.cwd.startsWith('/')) { send({ jsonrpc: '2.0', id: msg.id, error: { code: -32602, message: 'cwd must be absolute' } }); break; }
      reply({ sessionId: 'sess-1' });
      break;
    case 'session/prompt': {
      const sessionId = msg.params.sessionId;
      notify('session/update', { sessionId, update: { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'thinking' } } });
      notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call', toolCallId: 'call-1', title: 'edit_def', kind: 'other', status: 'pending', rawInput: { entity: 'validate', intent: 'refactor' } } });
      const outcome = await request('session/request_permission', {
        sessionId,
        toolCall: { toolCallId: 'call-1' },
        options: [
          { optionId: 'allow-once', name: 'Allow once', kind: 'allow_once' },
          { optionId: 'reject-once', name: 'Reject once', kind: 'reject_once' },
        ],
      });
      const allowed = outcome?.outcome?.outcome === 'selected' && outcome.outcome.optionId === 'allow-once';
      notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId: 'call-1', status: allowed ? 'completed' : 'failed', rawOutput: { ok: allowed } } });
      notify('session/update', { sessionId, update: { sessionUpdate: 'agent_message_chunk', messageId: 'm1', content: { type: 'text', text: allowed ? 'done' : 'rejected' } } });
      reply({ stopReason: 'end_turn' });
      break;
    }
    case 'session/cancel':
      break;
    default:
      if (msg.id !== undefined) send({ jsonrpc: '2.0', id: msg.id, error: { code: -32601, message: 'Method not found' } });
  }
});
rl.on('close', () => process.exit(0));
