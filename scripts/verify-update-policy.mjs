import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

function payload(manifest) {
  return Buffer.from([
    manifest.version, manifest.url, manifest.sha256.toLowerCase(),
    manifest.critical ? '1' : '0', manifest.min_version || '',
  ].join('\n'));
}

// Exercise the actual packaged program's read-only verifier. Neither the
// harness nor the program downloads, installs, starts capture or accesses keys.
export function verifyUpdatePolicy(executable, manifestPath, publicKeyPath) {
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const rawKey = Buffer.from(readFileSync(publicKeyPath, 'utf8').trim(), 'base64');
  assert.equal(rawKey.length, 32);
  const releaseKey = createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), rawKey]),
    type: 'spki', format: 'der',
  });
  assert.ok(verify(null, payload(manifest), releaseKey, Buffer.from(manifest.signature, 'base64')),
    'The input release manifest must have the fixed release signature');
  const parent = realpathSync(os.tmpdir());
  const directory = mkdtempSync(path.join(parent, 'carbonpaper-update-policy-'));
  try {
    const { privateKey, publicKey } = generateKeyPairSync('ed25519');
    const attackerKey = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
    const resultPath = path.join(directory, 'smoke-result.json');
    const env = {
      ...process.env,
      CARBONPAPER_UPDATE_SMOKE_TEST: '1',
      CARBONPAPER_UPDATE_SMOKE_PUBLIC_KEY: attackerKey,
      CARBONPAPER_UPDATE_MANIFEST_URL: 'http://127.0.0.1:1/latest.json',
      CARBONPAPER_UPDATE_SMOKE_RESULT: resultPath,
      CARBONPAPER_UPDATE_SMOKE_EXPECTED_VERSION: manifest.version,
    };
    const run = candidate => spawnSync(executable, ['--verify-update-manifest', candidate], {
      env, encoding: 'utf8', timeout: 15000, windowsHide: true,
    });
    const valid = run(manifestPath);
    assert.equal(valid.status, 0, valid.stderr || valid.error?.message);
    const hostile = { ...manifest, url: 'http://127.0.0.1:1/update.zip' };
    hostile.signature = sign(null, payload(hostile), privateKey).toString('base64');
    const hostilePath = path.join(directory, 'hostile.json');
    writeFileSync(hostilePath, JSON.stringify(hostile));
    const rejected = run(hostilePath);
    assert.equal(rejected.status, 1, rejected.stderr || rejected.error?.message);
    assert.match(rejected.stderr, /Update manifest signature verification failed/);
    assert.equal(existsSync(resultPath), false, 'Legacy smoke variables must not start a smoke runner');
    console.log('Packaged update verifier accepts the release key and rejects an environment-supplied key.');
  } finally {
    const resolved = realpathSync(directory);
    assert.equal(path.dirname(resolved), parent);
    assert.ok(path.basename(resolved).startsWith('carbonpaper-update-policy-'));
    rmSync(resolved, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 5) throw new Error('Usage: verify-update-policy.mjs <executable> <latest.json> <public-key>');
  verifyUpdatePolicy(...process.argv.slice(2));
}
