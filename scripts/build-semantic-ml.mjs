// Builds the isolated semantic worker and stages its pinned ONNX Runtime DLLs.
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';

const root = process.cwd();
const tauriDir = path.join(root, 'src-tauri');
const workerDir = path.join(tauriDir, 'semantic-worker');
const isRelease = process.argv.includes('--release');
const profile = isRelease ? 'release' : 'debug';
const manifest = JSON.parse(
  readFileSync(path.join(root, 'scripts', 'release-assets', 'onnxruntime-directml.json'), 'utf8'),
);

console.log(`Building carbonpaper-semantic-worker (${profile})...`);
const args = ['build', '--manifest-path', path.join(workerDir, 'Cargo.toml')];
if (isRelease) args.push('--release');
execFileSync('cargo', args, { cwd: root, stdio: 'inherit' });

const exe = path.join(workerDir, 'target', profile, 'carbonpaper-semantic-worker.exe');
if (!existsSync(exe)) throw new Error(`Semantic worker not found at ${exe}`);
const stagedExe = path.join(tauriDir, 'pre-bundle', 'carbonpaper-semantic-worker.exe');
mkdirSync(path.dirname(stagedExe), { recursive: true });
copyFileSync(exe, stagedExe);

const runtimeDir = path.join(tauriDir, 'pre-bundle', ...manifest.bundle_path.split('/'));
mkdirSync(runtimeDir, { recursive: true });
const localAppData = process.env.LOCALAPPDATA;
const legacyRuntimeDir = localAppData
  ? path.join(localAppData, 'carbonpaper', '.venv', 'Lib', 'site-packages', 'onnxruntime', 'capi')
  : null;

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

for (const pkg of manifest.packages) {
  for (const file of pkg.files) {
    const staged = path.join(runtimeDir, file.name);
    if (!existsSync(staged) && !isRelease && legacyRuntimeDir) {
      const legacy = path.join(legacyRuntimeDir, file.name);
      if (existsSync(legacy)) copyFileSync(legacy, staged);
    }
    if (!existsSync(staged)) {
      throw new Error(
        `Pinned semantic runtime asset is missing: ${staged}. Run npm run prepare:release-assets.`,
      );
    }
    const actual = sha256(staged);
    if (actual !== file.sha256) {
      // Development venv wheels may package byte-different DLLs from the pinned NuGet
      // artifacts. Keep those outside release staging and require the manifest for release.
      if (isRelease) {
        throw new Error(`${file.name} checksum mismatch: expected ${file.sha256}, got ${actual}`);
      }
      console.warn(`Development ${file.name} uses legacy wheel checksum ${actual}`);
    }
  }
}

// Clear any stale worker beside the standalone crate target; the bundled copy above is
// the single resource used by Tauri/portable packaging.
rmSync(path.join(tauriDir, 'target', profile, 'carbonpaper-semantic-worker.exe'), { force: true });
console.log(`Semantic worker staged at ${stagedExe}`);
