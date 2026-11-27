import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';

export const DEFAULT_CACHE_MAX_AGE_DAYS = 7;
const DAY_MS = 24 * 60 * 60 * 1000;
const DEVELOPMENT_DIRECTORY = /^development-[0-9a-f]{16}$/;

export function developmentInstanceId(workspace, { scope = 'interactive', localAppData = process.env.LOCALAPPDATA } = {}) {
  if (!localAppData) throw new Error('LOCALAPPDATA is required to identify development caches');
  return createHash('sha256')
    .update(`${realpathSync(workspace).toLowerCase()}\0${path.resolve(localAppData).toLowerCase()}\0${scope}`)
    .digest('hex').slice(0, 16);
}

export function rustCacheTargets(workspace) {
  const root = realpathSync(workspace);
  return [
    { directory: path.join(root, 'src-tauri'), package: 'carbonpaper' },
    { directory: path.join(root, 'src-tauri', 'app-bound'), package: 'carbonpaper-app-bound' },
    { directory: path.join(root, 'src-tauri', 'semantic-worker'), package: 'carbonpaper-semantic-worker' },
  ].map(target => ({ ...target, manifest: path.join(target.directory, 'Cargo.toml'), target: path.join(target.directory, 'target') }));
}

// Check every existing ancestor: junctions and symlinks must not redirect cleanup.
export function assertCachePath(workspace, candidate, { allowRoot = false } = {}) {
  const root = realpathSync(workspace);
  const absolute = path.resolve(candidate);
  const allowed = rustCacheTargets(root).some(({ target }) => {
    const relative = path.relative(target, absolute);
    return (allowRoot || relative !== '') && !path.isAbsolute(relative)
      && relative !== '..' && !relative.startsWith(`..${path.sep}`);
  });
  if (!allowed) throw new Error(`Refusing to clean outside a project build cache: ${absolute}`);
  let cursor = root;
  for (const component of path.relative(root, absolute).split(path.sep)) {
    cursor = path.join(cursor, component);
    let metadata;
    try { metadata = lstatSync(cursor); }
    catch (error) { if (error.code === 'ENOENT') break; throw error; }
    if (metadata.isSymbolicLink() || path.relative(cursor, realpathSync(cursor)) !== '') {
      throw new Error(`Refusing to clean a redirected build cache: ${cursor}`);
    }
  }
  return absolute;
}

export function activeDevelopmentTargets(workspace, options = {}) {
  const { target } = rustCacheTargets(workspace).find(item => item.package === 'carbonpaper-app-bound');
  return ['interactive', 'probe'].map(scope => path.join(target,
    `development-${developmentInstanceId(workspace, { ...options, scope })}`));
}

function lastModified(directory, cutoff) {
  let latest = lstatSync(directory).mtimeMs;
  if (latest >= cutoff) return latest;
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) continue;
    const file = path.join(directory, entry.name);
    latest = Math.max(latest, entry.isDirectory() ? lastModified(file, cutoff) : lstatSync(file).mtimeMs);
    if (latest >= cutoff) break;
  }
  return latest;
}

export function expiredDevelopmentCaches(workspace, {
  maxAgeDays = DEFAULT_CACHE_MAX_AGE_DAYS, now = Date.now(), localAppData = process.env.LOCALAPPDATA,
} = {}) {
  if (!Number.isInteger(maxAgeDays) || maxAgeDays < 0) throw new Error('Cache age must be a nonnegative integer');
  const { target } = rustCacheTargets(workspace).find(item => item.package === 'carbonpaper-app-bound');
  assertCachePath(workspace, target, { allowRoot: true });
  if (!existsSync(target)) return [];
  const active = new Set(activeDevelopmentTargets(workspace, { localAppData }));
  const cutoff = now - maxAgeDays * DAY_MS;
  return readdirSync(target, { withFileTypes: true }).flatMap(entry => {
    if (!DEVELOPMENT_DIRECTORY.test(entry.name)) return [];
    const directory = assertCachePath(workspace, path.join(target, entry.name));
    if (!entry.isDirectory() || active.has(directory)) return [];
    return lastModified(directory, cutoff) < cutoff ? [directory] : [];
  });
}

function runningRustBuilds() {
  if (process.platform !== 'win32') throw new Error('Rust cache cleanup requires Windows');
  return execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    "$ErrorActionPreference = 'Stop'; Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(cargo|rustc|clippy-driver|link|lld-link)\\.exe$' } | ForEach-Object { '{0} (PID {1})' -f $_.Name, $_.ProcessId }",
  ], { encoding: 'utf8', windowsHide: true }).trim();
}

export function assertNoRustBuilds() {
  const running = runningRustBuilds();
  if (running) throw new Error(`Wait for running Rust builds before cleaning caches: ${running}`);
}

export function removeCacheDirectory(workspace, directory) {
  const checked = assertCachePath(workspace, directory);
  rmSync(checked, { recursive: true, force: true });
}

export function pruneDevelopmentCaches(workspace, options = {}) {
  const expired = expiredDevelopmentCaches(workspace, options);
  if (expired.length && runningRustBuilds()) {
    console.log('[app-bound dev] Deferring expired cache cleanup until Rust builds finish.');
    return [];
  }
  for (const directory of expired) removeCacheDirectory(workspace, directory);
  return expired;
}

export function touchDevelopmentCache(workspace, directory) {
  assertCachePath(workspace, directory);
  mkdirSync(directory, { recursive: true });
  const marker = assertCachePath(workspace, path.join(directory, '.last-used'));
  writeFileSync(marker, `${new Date().toISOString()}\n`);
}

export function rustCleanupPlan(workspace, options = {}) {
  const { allDev = false } = options;
  const targets = rustCacheTargets(workspace);
  const appBound = targets.find(item => item.package === 'carbonpaper-app-bound');
  for (const target of activeDevelopmentTargets(workspace, options)) targets.push({ ...appBound, target });
  const cargo = [];
  const directories = [];
  for (const item of targets) {
    assertCachePath(workspace, item.target, { allowRoot: true });
    if (!existsSync(item.target)) continue;
    const base = ['clean', '--manifest-path', item.manifest, '--target-dir', item.target, '--offline'];
    const profiles = item.package === 'carbonpaper-app-bound' ? ['dev', 'dev-service'] : ['dev'];
    for (const profile of profiles) {
      const output = path.join(item.target, profile === 'dev' ? 'debug' : profile);
      assertCachePath(workspace, output);
      if (existsSync(output)) cargo.push([...base, '--profile', profile,
        ...(allDev ? [] : ['--package', item.package])]);
    }
    for (const profile of ['debug', 'dev-service', 'release']) {
      const incremental = assertCachePath(workspace, path.join(item.target, profile, 'incremental'));
      if (existsSync(incremental)) directories.push({ directory: incremental, reason: 'incremental cache' });
    }
  }
  for (const directory of expiredDevelopmentCaches(workspace, options)) {
    directories.push({ directory, reason: 'expired development instance' });
  }
  return { cargo, directories };
}
