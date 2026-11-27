import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { stageDevelopmentSemanticRuntime, verifySemanticRuntimeResources } from './semantic-runtime-resources.mjs';

function fixture(t) {
  const parent = realpathSync(os.tmpdir());
  const root = mkdtempSync(path.join(parent, 'carbonpaper-runtime-resources-'));
  t.after(() => {
    const resolved = realpathSync(root);
    assert.equal(path.dirname(resolved), parent);
    assert.ok(path.basename(resolved).startsWith('carbonpaper-runtime-resources-'));
    rmSync(resolved, { recursive: true, force: true });
  });
  const bundlePath = path.join('onnxruntime', '1.24.2');
  const staged = path.join(root, 'src-tauri', 'pre-bundle', bundlePath);
  const resources = path.join(root, 'src-tauri', 'target', 'debug');
  const target = path.join(resources, bundlePath);
  const manifestDirectory = path.join(root, 'scripts', 'release-assets');
  for (const directory of [staged, target, manifestDirectory]) mkdirSync(directory, { recursive: true });
  const files = ['onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'DirectML.dll'].map((name) => {
    const contents = Buffer.from(`pinned ${name}`);
    writeFileSync(path.join(staged, name), contents);
    return { name, size: contents.length, sha256: createHash('sha256').update(contents).digest('hex') };
  });
  writeFileSync(path.join(manifestDirectory, 'onnxruntime-directml.json'), JSON.stringify({
    bundle_path: bundlePath.replace(/\\/g, '/'), packages: [{ files }],
  }));
  return { root, staged, resources, target, files };
}

test('development resources restore missing and corrupt DLLs and preserve unchanged copies', (t) => {
  const { root, staged, resources, target, files } = fixture(t);
  writeFileSync(path.join(target, files[1].name), Buffer.alloc(files[1].size));
  const unchanged = path.join(target, files[2].name);
  writeFileSync(unchanged, readFileSync(path.join(staged, files[2].name)));
  const timestamp = new Date('2000-01-01T00:00:00Z');
  utimesSync(unchanged, timestamp, timestamp);

  stageDevelopmentSemanticRuntime(root);

  assert.equal(verifySemanticRuntimeResources(root, resources).length, 3);
  for (const file of files) {
    assert.deepEqual(readFileSync(path.join(target, file.name)), readFileSync(path.join(staged, file.name)));
  }
  assert.equal(statSync(unchanged).mtimeMs, timestamp.getTime());
});

test('development resources reject missing or tampered staged runtime files', (t) => {
  const { root, staged, files } = fixture(t);
  const runtime = path.join(staged, files[0].name);
  const contents = readFileSync(runtime);
  rmSync(runtime);
  assert.throws(() => stageDevelopmentSemanticRuntime(root), /resource is missing/);
  writeFileSync(runtime, Buffer.alloc(contents.length));
  assert.throws(() => stageDevelopmentSemanticRuntime(root), /checksum mismatch/);
});
