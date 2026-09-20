import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const demo = fileURLToPath(new URL('../demo/', import.meta.url));
const identity = {
  GIT_AUTHOR_NAME: 'Fixture', GIT_AUTHOR_EMAIL: 'fixture@localhost',
  GIT_COMMITTER_NAME: 'Fixture', GIT_COMMITTER_EMAIL: 'fixture@localhost',
};

function fixture(t, source, merged = true) {
  const root = mkdtempSync(join(tmpdir(), 'svc-git-twin-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const twin = join(root, 'git-twin');
  const work = join(twin, 'work');
  mkdirSync(join(work, 'src'), { recursive: true });
  cpSync(join(demo, 'git-twin/build.sh'), join(twin, 'build.sh'));
  writeFileSync(join(work, 'Cargo.toml'), '[package]\nname = "twin-fixture"\nversion = "0.1.0"\nedition = "2024"\n[workspace]\n');
  writeFileSync(join(work, 'src/main.rs'), source);
  const git = (...args) => {
    const result = spawnSync('git', ['-C', work, ...args], {
      encoding: 'utf8', env: { ...process.env, ...identity }, timeout: 10000,
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git('init', '-q', '-b', 'main');
  git('add', 'Cargo.toml', 'src/main.rs');
  const tree = git('write-tree');
  const base = git('commit-tree', tree, '-m', 'base');
  const left = git('commit-tree', tree, '-p', base, '-m', 'left');
  const right = git('commit-tree', tree, '-p', base, '-m', 'right');
  const head = merged ? git('commit-tree', tree, '-p', left, '-p', right, '-m', 'merge') : base;
  git('update-ref', 'refs/heads/main', head);
  return {
    work,
    run() {
      const result = spawnSync('bash', [join(twin, 'build.sh')], { encoding: 'utf8', timeout: 30000 });
      assert.ifError(result.error);
      assert.equal(result.signal, null);
      return result;
    },
  };
}

const prefix = 'fn read(_: &str) -> String { " raw ".into() }\nfn normalize(s: &str) -> String { s.trim().into() }\nfn log(s: &str) { println!("{s}"); }\nfn main() {\n';
const merged = `${prefix}    let raw = read("input");\n    let raw = normalize(&raw);\n    log(&raw);\n}\n`;

test('fresh git twin and its cached rerun both verify the shipped fixture', t => {
  const root = mkdtempSync(join(tmpdir(), 'svc-git-twin-fresh-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(join(demo, 'config'), join(root, 'config'), { recursive: true });
  mkdirSync(join(root, 'git-twin'));
  for (const file of ['build.sh', 'normalize.patch', 'logging.patch']) {
    cpSync(join(demo, 'git-twin', file), join(root, 'git-twin', file));
  }
  for (let pass = 0; pass < 2; pass++) {
    const result = spawnSync('bash', [join(root, 'git-twin/build.sh')], {
      encoding: 'utf8', timeout: 30000,
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /normalized shadow/);
  }
});

test('cached git twin accepts a compiled two-parent merge with the captured binding', t => {
  const { run } = fixture(t, merged);
  const result = run();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /binding bug|normalized shadow/);
});

test('cached git twin rejects a logging call before the shadow', t => {
  const source = `${prefix}    let raw = read("input");\n    log(&raw);\n    let raw = normalize(&raw);\n    println!("{raw}");\n}\n`;
  const { work, run } = fixture(t, source);
  const result = run();
  assert.notEqual(result.status, 0, result.stdout);
  assert.equal(readFileSync(join(work, 'src/main.rs'), 'utf8'), source);
});

test('cached git twin rejects a repository with no merge even if both strings appear', t => {
  const { run } = fixture(t, merged, false);
  const result = run();
  assert.notEqual(result.status, 0, result.stdout);
});

test('cached git twin rejects uncommitted edits without replacing them', t => {
  const { work, run } = fixture(t, merged);
  const changed = `${merged}\nfn unrelated() {}\n`;
  writeFileSync(join(work, 'src/main.rs'), changed);
  const result = run();
  assert.notEqual(result.status, 0, result.stdout);
  assert.equal(readFileSync(join(work, 'src/main.rs'), 'utf8'), changed);
});
