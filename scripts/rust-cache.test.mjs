import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { activeDevelopmentTargets, assertCachePath, DEFAULT_CACHE_MAX_AGE_DAYS, expiredDevelopmentCaches, removeCacheDirectory, rustCleanupPlan, touchDevelopmentCache } from './rust-cache.mjs';

function fixture(t) {
  const parent = realpathSync(os.tmpdir());
  const directory = mkdtempSync(path.join(parent, 'carbonpaper-rust-cache-'));
  const workspace = path.join(directory, 'workspace');
  const localAppData = path.join(directory, 'user-data');
  mkdirSync(workspace);
  t.after(() => {
    assert.equal(path.dirname(realpathSync(directory)), parent);
    assert.ok(path.basename(directory).startsWith('carbonpaper-rust-cache-'));
    rmSync(directory, { recursive: true, force: true });
  });
  return { directory, workspace, localAppData };
}

function cache(directory, date) {
  const output = path.join(directory, 'debug');
  mkdirSync(output, { recursive: true });
  const file = path.join(output, 'artifact.rlib');
  writeFileSync(file, 'fixture');
  for (const entry of [file, output, directory]) utimesSync(entry, date, date);
}

test('seven-day retention preserves both current identities and recently used old instances', t => {
  const { workspace, localAppData } = fixture(t);
  assert.equal(DEFAULT_CACHE_MAX_AGE_DAYS, 7);
  const target = path.join(workspace, 'src-tauri', 'app-bound', 'target');
  const now = Date.UTC(2026, 10, 14);
  const old = new Date(now - 8 * 86400000);
  const active = activeDevelopmentTargets(workspace, { localAppData });
  for (const directory of active) cache(directory, old);
  const expired = path.join(target, 'development-0000000000000001');
  const recent = path.join(target, 'development-0000000000000002');
  cache(expired, old);
  cache(recent, new Date(now - 6 * 86400000));
  const diagnostics = path.join(target, 'native-checks');
  cache(diagnostics, old);
  assert.deepEqual(expiredDevelopmentCaches(workspace, { localAppData, now }), [expired]);
  assert.deepEqual(expiredDevelopmentCaches(workspace, { localAppData, now, maxAgeDays: 0 }).sort(), [expired, recent].sort());
  removeCacheDirectory(workspace, expired);
  assert.ok(!existsSync(expired));
  for (const directory of [...active, recent, diagnostics]) assert.ok(existsSync(directory));
});

test('a recent nested artifact prevents an old unmarked instance from expiring', t => {
  const { workspace, localAppData } = fixture(t);
  const directory = path.join(workspace, 'src-tauri', 'app-bound', 'target', 'development-0000000000000001');
  const now = Date.UTC(2026, 10, 14);
  cache(directory, new Date(now - 8 * 86400000));
  utimesSync(path.join(directory, 'debug', 'artifact.rlib'), new Date(now), new Date(now));
  assert.deepEqual(expiredDevelopmentCaches(workspace, { localAppData, now }), []);
});

test('marking a reused development cache records its use without creating user data', t => {
  const { workspace, localAppData } = fixture(t);
  const [directory] = activeDevelopmentTargets(workspace, { localAppData });
  touchDevelopmentCache(workspace, directory);
  assert.ok(Number.isFinite(Date.parse(readFileSync(path.join(directory, '.last-used'), 'utf8').trim())));
  assert.ok(!existsSync(localAppData));
});

test('cleanup refuses target roots, sibling paths, and junctions to external data', t => {
  const { directory, workspace } = fixture(t);
  const target = path.join(workspace, 'src-tauri', 'target');
  const outside = path.join(directory, 'outside');
  mkdirSync(target, { recursive: true });
  mkdirSync(outside);
  writeFileSync(path.join(outside, 'keep.txt'), 'keep');
  assert.throws(() => removeCacheDirectory(workspace, target), /Refusing/);
  assert.throws(() => removeCacheDirectory(workspace, `${target}-other`), /Refusing/);
  assert.throws(() => removeCacheDirectory(workspace, outside), /Refusing/);
  symlinkSync(outside, path.join(target, 'debug'), process.platform === 'win32' ? 'junction' : 'dir');
  assert.throws(() => assertCachePath(workspace, path.join(target, 'debug', 'incremental')), /redirected/);
  assert.equal(readFileSync(path.join(outside, 'keep.txt'), 'utf8'), 'keep');
});

test('plans preserve dependency caches by default and release artifacts during a development reset', t => {
  const { workspace, localAppData } = fixture(t);
  for (const directory of ['src-tauri', 'src-tauri/app-bound', 'src-tauri/semantic-worker']) {
    for (const profile of ['debug', 'release']) mkdirSync(path.join(workspace, directory, 'target', profile, 'incremental'), { recursive: true });
  }
  mkdirSync(path.join(workspace, 'src-tauri', 'app-bound', 'target', 'dev-service'), { recursive: true });
  const plan = rustCleanupPlan(workspace, { localAppData });
  assert.equal(plan.cargo.length, 4);
  assert.ok(plan.cargo.every(args => args.includes('--package') && !args.includes('release')));
  assert.equal(plan.directories.length, 6);
  const reset = rustCleanupPlan(workspace, { localAppData, allDev: true });
  assert.equal(reset.cargo.length, 4);
  assert.ok(reset.cargo.every(args => !args.includes('--package') && !args.includes('release')));
  assert.ok(reset.cargo.some(args => args.includes('dev-service')));
  for (const item of plan.directories) assert.equal(path.basename(item.directory), 'incremental');
});
