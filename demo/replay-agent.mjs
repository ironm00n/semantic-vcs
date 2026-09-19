// A scripted stand-in for the model: an ACP v1 agent over stdio that replays a recording
// of svc ops (demo/recordings/line9.ops.jsonl; line9.jsonl is the live run's summary) through the real `svc` binary, so the TUI hosts a
// run with no API key (SPEC §10 line 12 rehearsal). Each op becomes a tool_call; edit_def
// asks permission first, exactly as the review queue expects; the ops land in whatever
// changeset the host opened.
//
//   SVC_AGENT_COMMAND="node demo/replay-agent.mjs demo/recordings/line9.ops.jsonl" \
//     svc tui --agent "rename read to read_file and pull the retry check out of validate"
//
// Recording lines: {"op": "rename", "entity": "read", "new_name": "read_file"} — keys map to
// `svc <op> --key value`. An edit_def may carry `patch: {find, replace}` instead of a full
// `definition`; the replacement is applied to the entity's current source in the working copy.
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import { spawnSync } from 'node:child_process';

const recording = process.argv[2];
if (!recording) { process.stderr.write('usage: replay-agent.mjs <recording.jsonl>\n'); process.exit(2); }
const svcBin = process.env.SVC_BIN ?? 'svc';
const ops = fs.readFileSync(recording, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));

const rl = readline.createInterface({ input: process.stdin });
const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
let nextId = 100;
const pending = new Map();
const request = (method, params) =>
  new Promise((resolve) => { const id = nextId++; pending.set(id, resolve); send({ jsonrpc: '2.0', id, method, params }); });
const notify = (method, params) => send({ jsonrpc: '2.0', method, params });
let cancelled = false;

process.stderr.write(`replay agent up: ${ops.length} ops from ${recording}\n`);

// The item's current source, like demo-lines.sh's `item()`: from `fn name(` to the first `}` at column 0.
function itemSource(cwd, name) {
  const src = fs.readFileSync(path.join(cwd, 'src/main.rs'), 'utf8').split('\n');
  const start = src.findIndex((l) => l.startsWith(`fn ${name}(`));
  if (start < 0) throw new Error(`no fn ${name} in src/main.rs`);
  const end = src.findIndex((l, i) => i > start && l === '}');
  return src.slice(start, end + 1).join('\n') + '\n';
}

function runSvc(cwd, op) {
  const { op: verb, note, patch, ...args } = op;
  if (verb === 'edit_def' && patch && args.definition === undefined) {
    const cur = itemSource(cwd, args.entity);
    if (!cur.includes(patch.find)) throw new Error(`patch.find not in ${args.entity}: ${patch.find}`);
    args.definition = cur.replace(patch.find, patch.replace);
  }
  const argv = [verb.replace(/_/g, '-')];
  for (const [k, v] of Object.entries(args)) argv.push(`--${k.replace(/_/g, '-')}`, String(v));
  argv.push('--json');
  const r = spawnSync(svcBin, argv, { cwd, encoding: 'utf8' });
  let out = null;
  try { out = JSON.parse(r.stdout); } catch { out = { stdout: r.stdout }; }
  if (r.status !== 0) throw new Error(`svc ${argv[0]} exited ${r.status}: ${r.stderr.trim()}`);
  return { args, out };
}

rl.on('line', async (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.id !== undefined && msg.method === undefined) { pending.get(msg.id)?.(msg.result ?? msg.error); pending.delete(msg.id); return; }
  const reply = (result) => send({ jsonrpc: '2.0', id: msg.id, result });
  switch (msg.method) {
    case 'initialize':
      reply({ protocolVersion: 1, agentCapabilities: {}, authMethods: [], agentInfo: { name: 'svc-replay', version: '0' } });
      break;
    case 'session/new':
      if (!path.isAbsolute(msg.params.cwd)) { send({ jsonrpc: '2.0', id: msg.id, error: { code: -32602, message: 'cwd must be absolute' } }); break; }
      cwd = msg.params.cwd;
      reply({ sessionId: 'replay-1' });
      break;
    case 'session/prompt': {
      const sessionId = msg.params.sessionId;
      const text = (t) => ({ type: 'text', text: t });
      notify('session/update', { sessionId, update: { sessionUpdate: 'agent_thought_chunk', content: text(`replaying ${ops.length} recorded ops`) } });
      let done = 0;
      for (const [i, op] of ops.entries()) {
        if (cancelled) break;
        const toolCallId = `replay-${i + 1}`;
        const { note, patch, ...shown } = op;
        notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call', toolCallId, title: op.op, kind: 'edit', status: 'pending', rawInput: shown } });
        if (op.op === 'edit_def') {
          const outcome = await request('session/request_permission', {
            sessionId,
            toolCall: { toolCallId },
            options: [
              { optionId: 'allow-once', name: 'Allow once', kind: 'allow_once' },
              { optionId: 'reject-once', name: 'Reject once', kind: 'reject_once' },
            ],
          });
          const allowed = outcome?.outcome?.outcome === 'selected' && outcome.outcome.optionId === 'allow-once';
          if (!allowed) {
            notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId, status: 'failed', rawOutput: { rejected: true } } });
            continue;
          }
        }
        // Pause so the ops visibly stream into the pane at the expo instead of landing at once.
        await new Promise((r) => setTimeout(r, 700));
        try {
          const { out } = runSvc(cwd, op);
          done += 1;
          notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId, status: 'completed', rawOutput: out } });
        } catch (e) {
          notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId, status: 'failed', rawOutput: { error: String(e.message ?? e) } } });
        }
      }
      notify('session/update', { sessionId, update: { sessionUpdate: 'agent_message_chunk', messageId: 'replay', content: text(`${done} of ${ops.length} recorded ops applied`) } });
      reply({ stopReason: cancelled ? 'cancelled' : 'end_turn' });
      break;
    }
    case 'session/cancel':
      cancelled = true;
      break;
    default:
      if (msg.id !== undefined) send({ jsonrpc: '2.0', id: msg.id, error: { code: -32601, message: 'Method not found' } });
  }
});
let cwd = process.cwd();
rl.on('close', () => process.exit(0));
