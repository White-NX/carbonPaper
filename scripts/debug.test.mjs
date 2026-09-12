import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { developmentConfiguration, tauriDevArguments } from './debug.mjs';

test('development service identity is stable within a workspace and separates probes and other workspaces', (t) => {
  const parent = realpathSync(os.tmpdir());
  const directory = mkdtempSync(path.join(parent, 'carbonpaper-dev-config-'));
  t.after(() => {
    const resolved = realpathSync(directory);
    assert.equal(path.dirname(resolved), parent);
    assert.ok(path.basename(resolved).startsWith('carbonpaper-dev-config-'));
    rmSync(resolved, { recursive: true, force: true });
  });
  const workspace = path.join(directory, 'workspace');
  const other = path.join(directory, 'other');
  const localAppData = path.join(directory, 'user-data');
  mkdirSync(workspace); mkdirSync(other);
  const first = developmentConfiguration(workspace, { localAppData });
  const again = developmentConfiguration(workspace, { localAppData });
  const probe = developmentConfiguration(workspace, { localAppData, scope: 'probe' });
  const second = developmentConfiguration(other, { localAppData });
  const otherUser = developmentConfiguration(workspace, { localAppData: path.join(directory, 'another-user') });
  assert.match(first.instance, /^[0-9a-f]{16}$/);
  assert.equal(first.instance, again.instance);
  assert.equal(first.publicKey, again.publicKey);
  assert.notEqual(first.instance, probe.instance);
  assert.notEqual(first.instance, second.instance);
  assert.notEqual(first.instance, otherUser.instance);
  assert.notEqual(first.publicKey, probe.publicKey);
  assert.ok(first.directory.startsWith(localAppData + path.sep));
  assert.equal(Buffer.from(readFileSync(path.join(first.directory, 'signing-seed.txt'), 'utf8'), 'base64').length, 32);
  assert.equal(first.environment.CARBONPAPER_APP_BOUND_DEV, '1');
});

test('the ordinary debug entry point enables app-bound with the existing Tauri configuration', () => {
  const packageJson = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
  assert.match(packageJson.scripts.debug, /node scripts\/debug\.mjs/);
  assert.equal(packageJson.scripts['debug:app-bound'], undefined);
  const args = tauriDevArguments('workspace', ['--no-watch']);
  assert.deepEqual(args.slice(1), ['dev', '--features', 'app-bound-dev', '--no-watch']);
  assert.ok(!args.includes('--config'));
});
