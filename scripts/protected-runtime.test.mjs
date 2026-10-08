import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { manifestName, signatureName, signingContext, safeRuntimePath, signProtectedRuntime, verifyProtectedRuntime } from './protected-runtime.mjs';
import { verifyPrivilegedImports } from './privileged-imports.mjs';

const binaries = ['carbonpaper.exe', 'carbonpaper-ml.exe', 'carbonpaper-office.exe', 'carbonpaper-nmh.exe'];

function testPe(importedDll = 'kernel32.dll') {
  const bytes = Buffer.alloc(1024);
  bytes.writeUInt16LE(0x5a4d); bytes.writeUInt32LE(0x80, 0x3c);
  bytes.writeUInt32LE(0x4550, 0x80); bytes.writeUInt16LE(0x8664, 0x84);
  bytes.writeUInt16LE(1, 0x86); bytes.writeUInt16LE(240, 0x94);
  bytes.writeUInt16LE(0x20b, 0x98); bytes.writeUInt32LE(16, 0x98 + 108);
  bytes.writeUInt32LE(0x1000, 0x98 + 120); bytes.writeUInt32LE(40, 0x98 + 124);
  bytes.writeUInt32LE(0x1000, 0x188 + 12); bytes.writeUInt32LE(0x200, 0x188 + 16); bytes.writeUInt32LE(0x200, 0x188 + 20);
  bytes.writeUInt32LE(0x1040, 0x200 + 12); bytes.write(importedDll, 0x240, 'ascii');
  return bytes;
}

async function fixture(t) {
  const parent = realpathSync(os.tmpdir());
  const root = mkdtempSync(path.join(parent, 'carbonpaper-runtime-test-with spaces-'));
  t.after(() => {
    const resolved = realpathSync(root);
    assert.equal(path.dirname(resolved), parent);
    assert.ok(path.basename(resolved).startsWith('carbonpaper-runtime-test-'));
    rmSync(resolved, { recursive: true, force: true });
  });
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const trustedKey = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
  const signingKey = Buffer.from(privateKey.export({ type: 'pkcs8', format: 'pem' })).toString('base64');
  const tauri = path.join(root, 'src-tauri');
  const prebundle = path.join(tauri, 'pre-bundle');
  const release = path.join(tauri, 'target', 'release');
  const packaged = path.join(root, 'package');
  mkdirSync(prebundle, { recursive: true });
  mkdirSync(release, { recursive: true });
  writeFileSync(path.join(tauri, 'tauri.conf.json'), JSON.stringify({ version: '0.8.5-test.1+build.2' }));
  writeFileSync(path.join(tauri, 'update-public-key.txt'), trustedKey);
  for (const name of binaries) writeFileSync(path.join(release, name), `synthetic test binary: ${name}`);
  for (const name of ['carbonpaper-semantic-worker.exe', 'carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe']) {
    writeFileSync(path.join(prebundle, name), name === 'carbonpaper-key-service.exe' || name === 'carbonpaper-protected-setup.exe'
      ? testPe() : `synthetic resource: ${name}`);
  }
  mkdirSync(path.join(prebundle, 'onnxruntime', '1.24.2'), { recursive: true });
  writeFileSync(path.join(prebundle, 'onnxruntime', '1.24.2', 'onnxruntime.dll'), 'synthetic model runtime');
  const manifest = await signProtectedRuntime(root, signingKey);
  cpSync(prebundle, packaged, { recursive: true });
  for (const name of binaries) cpSync(path.join(release, name), path.join(packaged, name));
  const resign = (next, context = signingContext) => {
    const bytes = Buffer.from(`${JSON.stringify(next)}\n`);
    writeFileSync(path.join(packaged, manifestName), bytes);
    writeFileSync(path.join(packaged, signatureName), sign(null, Buffer.concat([context, bytes]), privateKey).toString('base64'));
  };
  return { root, prebundle, release, packaged, signingKey, trustedKey, manifest, resign };
}

function stageSigningScripts(root) {
  const scripts = path.join(root, 'scripts');
  mkdirSync(scripts);
  for (const name of ['sign-protected-runtime.mjs', 'protected-runtime.mjs', 'privileged-imports.mjs']) {
    cpSync(new URL(name, import.meta.url), path.join(scripts, name));
  }
  cpSync(new URL('../src-tauri/nsis_hooks.nsh', import.meta.url), path.join(root, 'src-tauri', 'nsis_hooks.nsh'));
  return path.join(scripts, 'sign-protected-runtime.mjs');
}

test('the signing entry point works from the NSIS compiler directory', async (t) => {
  const f = await fixture(t);
  const script = stageSigningScripts(f.root);
  const compilerDirectory = path.join(f.release, 'nsis', 'x64');
  mkdirSync(compilerDirectory, { recursive: true });
  const result = spawnSync(process.execPath, [script], {
    cwd: compilerDirectory,
    env: { ...process.env, CARBONPAPER_UPDATE_SIGNING_KEY: f.signingKey },
    encoding: 'utf8',
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr || result.error?.message);
  cpSync(f.prebundle, f.packaged, { recursive: true });
  assert.deepEqual(await verifyProtectedRuntime(f.packaged, f.trustedKey), f.manifest);
});

const nsisCompiler = process.platform === 'win32' && [
  process.env.LOCALAPPDATA && path.join(process.env.LOCALAPPDATA, 'tauri', 'NSIS', 'makensis.exe'),
  process.env['ProgramFiles(x86)'] && path.join(process.env['ProgramFiles(x86)'], 'NSIS', 'makensis.exe'),
  process.env.ProgramFiles && path.join(process.env.ProgramFiles, 'NSIS', 'makensis.exe'),
].find(candidate => candidate && existsSync(candidate));
const canExtractNsis = nsisCompiler && spawnSync('7z', ['i'], { stdio: 'ignore', windowsHide: true }).status === 0;

test('NSIS packages a fresh signature after Tauri patches the executable and aborts if signing fails', {
  skip: canExtractNsis ? false : 'Requires Windows with NSIS and 7-Zip',
}, async (t) => {
  const f = await fixture(t);
  stageSigningScripts(f.root);
  const main = path.join(f.release, 'carbonpaper.exe');
  writeFileSync(main, 'synthetic executable after the Tauri NSIS bundle-type patch');
  cpSync(main, path.join(f.packaged, 'carbonpaper.exe'));
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /checksum mismatch: carbonpaper\.exe/);

  const compilerDirectory = path.join(f.release, 'nsis', 'x64');
  mkdirSync(compilerDirectory, { recursive: true });
  const installer = path.join(compilerDirectory, 'runtime.exe');
  const script = path.join(compilerDirectory, 'installer.nsi');
  writeFileSync(script, [
    'Unicode true',
    `!include "${path.join(f.root, 'src-tauri', 'nsis_hooks.nsh')}"`,
    'Name "Protected runtime regression"',
    `OutFile "${installer}"`,
    'RequestExecutionLevel user',
    'SetCompressor /SOLID lzma',
    'Section',
    'SetOutPath $INSTDIR',
    ...binaries.map(name => `File "${path.join(f.release, name)}"`),
    `File /r "${f.prebundle}\\*"`,
    'SectionEnd',
    '',
  ].join('\n'));
  const compile = signingKey => spawnSync(nsisCompiler, ['/V2', script], {
    cwd: compilerDirectory,
    env: { ...process.env, CARBONPAPER_UPDATE_SIGNING_KEY: signingKey },
    encoding: 'utf8',
    windowsHide: true,
  });
  const built = compile(f.signingKey);
  assert.equal(built.status, 0, built.stdout + built.stderr);
  const extracted = path.join(f.root, 'extracted installer');
  const extraction = spawnSync('7z', ['x', installer, `-o${extracted}`, '-y'], { encoding: 'utf8', windowsHide: true });
  assert.equal(extraction.status, 0, extraction.stdout + extraction.stderr);
  const verified = await verifyProtectedRuntime(extracted, f.trustedKey);
  assert.notEqual(verified.files['carbonpaper.exe'], f.manifest.files['carbonpaper.exe']);

  const missingKey = compile('');
  assert.notEqual(missingKey.status, 0);
  assert.match(missingKey.stdout + missingKey.stderr, /CARBONPAPER_UPDATE_SIGNING_KEY is required/);
});

test('final packaged binaries and nested resources verify with the release key', async (t) => {
  const f = await fixture(t);
  const verified = await verifyProtectedRuntime(f.packaged, f.trustedKey);
  assert.deepEqual(verified, f.manifest);
  assert.ok(verified.files['carbonpaper-nmh.exe']);
  assert.ok(verified.files['onnxruntime/1.24.2/onnxruntime.dll']);
});

test('a changed executable is rejected even with an intact signature', async (t) => {
  const f = await fixture(t);
  writeFileSync(path.join(f.packaged, 'carbonpaper.exe'), 'replaced executable');
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /checksum mismatch/);
});

test('manifest signature uses a separate domain and covers exact bytes', async (t) => {
  const f = await fixture(t);
  f.resign(f.manifest, Buffer.alloc(0));
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /signature/);
  f.resign(f.manifest);
  const file = path.join(f.packaged, manifestName);
  writeFileSync(file, Buffer.concat([readFileSync(file), Buffer.from(' ')]));
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /signature/);
});

test('missing and mismatched signing keys fail without substituting a development key', async (t) => {
  const f = await fixture(t);
  await assert.rejects(signProtectedRuntime(f.root), /required/);
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const other = Buffer.from(privateKey.export({ type: 'pkcs8', format: 'pem' })).toString('base64');
  await assert.rejects(signProtectedRuntime(f.root, other), /does not match/);
  const otherPublic = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
  await assert.rejects(verifyProtectedRuntime(f.packaged, otherPublic), /signature/);
});

test('signed traversal, device, ADS and Windows alias paths are rejected', async (t) => {
  const f = await fixture(t);
  for (const relative of ['../outside', 'C:/outside', '//server/share', 'a\\b', 'a:stream',
    'a/CON.dll', 'LPT1.txt', 'COM¹.txt', 'a./b', 'a /b', 'a//b', 'a/../b', 'a\u0001b']) {
    assert.throws(() => safeRuntimePath(relative), /Unsafe/);
    f.resign({ ...f.manifest, files: { ...f.manifest.files, [relative]: 'a'.repeat(64) } });
    await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /Unsafe/);
  }
});

test('case-folded duplicate paths are rejected both when signing and when verifying', async (t) => {
  const f = await fixture(t);
  f.resign({ ...f.manifest, files: { ...f.manifest.files, 'CarbonPaper.exe': f.manifest.files['carbonpaper.exe'] } });
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /Duplicate Windows/);
  writeFileSync(path.join(f.prebundle, 'CarbonPaper.exe'), 'duplicate name in another build output');
  await assert.rejects(signProtectedRuntime(f.root, f.signingKey), /Duplicate Windows/);
});

test('a linked parent directory is rejected even when its file hash matches', async (t) => {
  const f = await fixture(t);
  const target = path.join(f.root, 'linked-target');
  mkdirSync(target);
  const contents = Buffer.from('matching but redirected resource');
  writeFileSync(path.join(target, 'model.dll'), contents);
  symlinkSync(target, path.join(f.packaged, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  f.resign({ ...f.manifest, files: { ...f.manifest.files, 'linked/model.dll': createHash('sha256').update(contents).digest('hex') } });
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /symbolic link/);
  symlinkSync(target, path.join(f.prebundle, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(signProtectedRuntime(f.root, f.signingKey), /symbolic link/);
});

test('oversized metadata, invalid versions, hashes and missing components are rejected', async (t) => {
  const f = await fixture(t);
  writeFileSync(path.join(f.packaged, manifestName), Buffer.alloc(1024 * 1024 + 1));
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /Oversized/);
  for (const version of ['', '../1.2.3', '1.2', '1.2.3-01', '01.2.3']) {
    f.resign({ ...f.manifest, version });
    await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /Unsupported/);
  }
  f.resign({ ...f.manifest, files: { ...f.manifest.files, 'carbonpaper-semantic-worker.exe': 'not-a-hash' } });
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /checksum/);
  const files = { ...f.manifest.files }; delete files['carbonpaper-nmh.exe'];
  f.resign({ ...f.manifest, files });
  await assert.rejects(verifyProtectedRuntime(f.packaged, f.trustedKey), /missing carbonpaper-nmh/);
});

test('privileged helper imports reject a private CRT and malformed PE headers', async (t) => {
  const f = await fixture(t);
  const helper = path.join(f.prebundle, 'carbonpaper-protected-setup.exe');
  assert.deepEqual(verifyPrivilegedImports(helper), ['kernel32.dll']);
  writeFileSync(helper, testPe('vcruntime140.dll'));
  await assert.rejects(signProtectedRuntime(f.root, f.signingKey), /non-system DLL/);
  writeFileSync(helper, Buffer.from('invalid PE'));
  assert.throws(() => verifyPrivilegedImports(helper), /privileged PE/);
});
