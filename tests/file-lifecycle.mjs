import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const svc = resolve(process.env.SVC_BIN ?? fileURLToPath(new URL('../target/debug/svc', import.meta.url)));

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'svc-file-lifecycle-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (path, bytes) => {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), bytes);
  };
  const invoke = (...args) => {
    const result = spawnSync(svc, ['--json', ...args], {
      cwd: root, encoding: 'utf8', timeout: 15000,
    });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    return result;
  };
  const run = (...args) => {
    const result = invoke(...args);
    assert.equal(result.status, 0, `svc ${args.join(' ')}: ${result.stderr}\n${result.stdout}`);
    return JSON.parse(result.stdout);
  };
  put('src/lib.rs', 'pub fn value() -> u8 { 1 }\n');
  return { root, put, run, invoke };
}

function files(root) {
  const result = {};
  function walk(dir, prefix = '') {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === '.svc' || entry.name === '.svc-workspace') continue;
      const name = prefix + entry.name;
      if (entry.isDirectory()) walk(join(dir, entry.name), `${name}/`);
      else result[name] = readFileSync(join(dir, entry.name)).toString('hex');
    }
  }
  walk(root);
  return result;
}

function restoreRoundTrip(run, root, before, after) {
  const index = run('op', 'log')[0].ix;
  run('undo');
  assert.deepEqual(files(root), before);
  assert.equal(run('status').absorbed, false);
  run('op', 'restore', String(index));
  assert.deepEqual(files(root), after);
  assert.equal(run('status').absorbed, false);
  assert.equal(run('replay').diverged_at, null);
}

test('opaque binary and empty files survive absorb, undo, restore, and a store-only checkout', t => {
  const { root, put, run } = fixture(t);
  put('assets/data.bin', Buffer.from([0, 255, 128, 13, 10]));
  put('assets/empty', Buffer.alloc(0));
  put('obsolete.txt', 'remove me\n');
  const before = files(root);
  run('init');
  put('assets/data.bin', Buffer.from([255, 0, 1, 2]));
  put('assets/new-empty', Buffer.alloc(0));
  rmSync(join(root, 'obsolete.txt'));
  const after = files(root);
  assert.equal(run('status').absorbed, true);
  restoreRoundTrip(run, root, before, after);
  const checkout = mkdtempSync(join(tmpdir(), 'svc-file-lifecycle-checkout-'));
  t.after(() => rmSync(checkout, { recursive: true, force: true }));
  run('workspace', 'add', 'copy', checkout);
  assert.deepEqual(files(checkout), after);
});

test('undo preserves ignored files next to a rewritten tracked file', t => {
  const { root, put, run } = fixture(t);
  put('.svcignore', 'assets/data.bin.svc-tmp\n');
  put('assets/data.bin', 'original bytes\n');
  put('assets/data.bin.svc-tmp', 'untracked bytes must survive\n');
  const before = files(root);
  run('init');
  put('assets/data.bin', 'changed bytes\n');
  const after = files(root);
  assert.equal(run('status').absorbed, true);
  restoreRoundTrip(run, root, before, after);
});

test('undo and restore replace a directory with the original file', t => {
  const { root, put, run } = fixture(t);
  put('payload', 'original file\n');
  const before = files(root);
  run('init');
  rmSync(join(root, 'payload'));
  put('payload/nested/data.bin', Buffer.from([0, 255]));
  const after = files(root);
  assert.equal(run('status').absorbed, true);
  restoreRoundTrip(run, root, before, after);
});

test('a blocking directory containing ignored data is refused without deleting that data', t => {
  const { root, put, run, invoke } = fixture(t);
  put('.svcignore', 'kept.cache\n');
  put('payload', 'original file\n');
  run('init');
  rmSync(join(root, 'payload'));
  put('payload/nested/data.bin', Buffer.from([0, 255]));
  put('payload/nested/kept.cache', 'untracked bytes must survive\n');
  assert.equal(run('status').absorbed, true);
  const result = invoke('undo');
  assert.notEqual(result.status, 0, 'a file cannot replace a directory holding ignored data');
  assert.equal(readFileSync(join(root, 'payload/nested/kept.cache'), 'utf8'), 'untracked bytes must survive\n');
});

test('undo and restore replace a file with the original directory', t => {
  const { root, put, run } = fixture(t);
  put('payload/nested/data.bin', Buffer.from([0, 255]));
  const before = files(root);
  run('init');
  rmSync(join(root, 'payload'), { recursive: true });
  put('payload', 'replacement file\n');
  const after = files(root);
  assert.equal(run('status').absorbed, true);
  restoreRoundTrip(run, root, before, after);
});
