import { spawn, spawnSync } from 'node:child_process';
import { createHash, createPrivateKey, createPublicKey, randomBytes } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { verifyPrivilegedImports } from './privileged-imports.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
export const DEVELOPMENT_NATIVE_PROFILE = 'dev-service';

export function nativeCargoArguments(command, workspace, args = []) {
  return [command, '--manifest-path', path.join(workspace, 'src-tauri', 'app-bound', 'Cargo.toml'),
    '--profile', DEVELOPMENT_NATIVE_PROFILE, '--features', 'development-runtime', ...args];
}

export function tauriDevArguments(workspace, args = []) {
  return [path.join(workspace, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'),
    'dev', '--features', 'app-bound-dev', ...args];
}

export function developmentConfiguration(workspace, { scope = 'interactive', localAppData = process.env.LOCALAPPDATA } = {}) {
  if (!localAppData) throw new Error('LOCALAPPDATA is required for the Windows development instance');
  const canonical = realpathSync(workspace);
  const instance = createHash('sha256').update(`${canonical.toLowerCase()}\0${path.resolve(localAppData).toLowerCase()}\0${scope}`)
    .digest('hex').slice(0, 16);
  const directory = path.join(localAppData, `CarbonPaperDev-${instance}`);
  mkdirSync(directory, { recursive: true });
  const seedPath = path.join(directory, 'signing-seed.txt');
  if (!existsSync(seedPath)) {
    try { writeFileSync(seedPath, randomBytes(32).toString('base64'), { flag: 'wx', mode: 0o600 }); }
    catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  const seed = Buffer.from(readFileSync(seedPath, 'utf8').trim(), 'base64');
  if (seed.length !== 32) throw new Error('Invalid development signing seed; preserve the existing instance key');
  const key = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]), type: 'pkcs8', format: 'der' });
  seed.fill(0);
  const publicKey = createPublicKey(key).export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
  return { instance, directory, publicKey, workspace: canonical, environment: {
    ...process.env,
    CARBONPAPER_APP_BOUND_DEV: '1',
    CARBONPAPER_APP_BOUND_DEV_INSTANCE: instance,
    CARBONPAPER_APP_BOUND_DEV_PUBLIC_KEY: publicKey,
  } };
}

function run(program, args, environment, capture = false) {
  const result = spawnSync(program, args, { cwd: root, env: environment, windowsHide: true,
    encoding: 'utf8', stdio: ['ignore', capture ? 'pipe' : 'inherit', 'inherit'] });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${path.basename(program)} failed (${result.status ?? result.signal})`);
  return result.stdout || '';
}

function rustFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const full = path.join(directory, entry.name);
    return entry.isDirectory() ? rustFiles(full) : entry.name.endsWith('.rs') ? [full] : [];
  });
}

function sha256(file) { return createHash('sha256').update(readFileSync(file)).digest('hex'); }

export function prepareNativeComponents(configuration) {
  const crate = path.join(configuration.workspace, 'src-tauri', 'app-bound');
  const target = path.join(crate, 'target', `development-${configuration.instance}`);
  const environment = { ...configuration.environment, CARGO_TARGET_DIR: target,
    RUSTFLAGS: `${configuration.environment.RUSTFLAGS || ''} -C target-feature=+crt-static`.trim() };
  delete environment.CARGO_ENCODED_RUSTFLAGS;
  const buildArgs = nativeCargoArguments('build', configuration.workspace,
    ['--bin', 'carbonpaper-key-service', '--bin', 'carbonpaper-protected-setup']);
  const hash = createHash('sha256').update(configuration.instance).update(configuration.publicKey)
    .update(environment.RUSTFLAGS).update(JSON.stringify(buildArgs))
    .update(run('rustc', ['-vV'], environment, true));
  for (const file of [...rustFiles(path.join(crate, 'src')), path.join(crate, 'Cargo.toml'),
    path.join(crate, 'Cargo.lock'), path.join(crate, 'build.rs')].sort()) {
    hash.update(file).update(readFileSync(file));
  }
  const fingerprint = hash.digest('hex');
  const components = path.join(configuration.directory, 'components');
  const statePath = path.join(components, 'build-state.json');
  let previous;
  try { previous = JSON.parse(readFileSync(statePath, 'utf8')); } catch { previous = null; }
  const names = ['carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe'];
  const outputDirectory = path.join(target, DEVELOPMENT_NATIVE_PROFILE);
  if (previous?.fingerprint !== fingerprint || names.some(name => !existsSync(path.join(components, name))
    || sha256(path.join(components, name)) !== previous.files?.[name])) {
    console.log('[app-bound dev] Building isolated service and setup helper...');
    run('cargo', buildArgs, environment);
    mkdirSync(components, { recursive: true });
    const files = {};
    for (const name of names) {
      const binary = path.join(outputDirectory, name);
      verifyPrivilegedImports(binary);
      files[name] = sha256(binary);
      if (!existsSync(path.join(components, name)) || sha256(path.join(components, name)) !== files[name]) {
        copyFileSync(binary, path.join(components, name));
      }
    }
    writeFileSync(statePath, `${JSON.stringify({ fingerprint, files }, null, 2)}\n`);
  }
  return { target, outputDirectory, environment };
}

async function main() {
  if (process.platform !== 'win32') throw new Error('The app-bound development instance requires Windows');
  const args = process.argv.slice(2);
  const mode = args[0]?.startsWith('--') && ['--prepare-native', '--smoke', '--test-core'].includes(args[0]) ? args.shift() : 'dev';
  const configuration = developmentConfiguration(root, { scope: mode === '--smoke' ? 'probe' : 'interactive' });
  if (process.env.CARBONPAPER_APP_BOUND_DEV_INSTANCE && process.env.CARBONPAPER_APP_BOUND_DEV_INSTANCE !== configuration.instance) {
    throw new Error('The requested development instance does not match this workspace');
  }
  const native = prepareNativeComponents(configuration);
  if (mode === '--prepare-native') return;
  if (mode === '--test-core') {
    run('cargo', nativeCargoArguments('test', root, ['--lib']), native.environment);
    return;
  }
  if (mode === '--smoke') {
    run('cargo', nativeCargoArguments('build', root, ['--bin', 'carbonpaper-development-probe']), native.environment);
    const probe = path.join(configuration.directory, 'probe', 'carbonpaper.exe');
    mkdirSync(path.dirname(probe), { recursive: true });
    copyFileSync(path.join(native.outputDirectory, 'carbonpaper-development-probe.exe'), probe);
    run(probe, [], configuration.environment);
    return;
  }
  console.log(`[app-bound dev] Service registration: ${configuration.instance}`);
  const child = spawn(process.execPath, tauriDevArguments(root, args),
    { cwd: root, env: configuration.environment, stdio: 'inherit', windowsHide: true });
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
    if (child.exitCode === null && child.pid) spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  });
  const status = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', code => resolve(code ?? 1)); });
  process.exitCode = status;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(`[app-bound dev] ${error.message}`); process.exitCode = 1; });
}
