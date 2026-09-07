// Builds development workers only when their sources or staged outputs changed.
// The three workers in src-tauri share one Cargo package, so keeping them in a
// single Cargo invocation avoids rebuilding the package setup three times.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
  renameSync,
} from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const tauriDir = path.join(root, 'src-tauri');
const profile = 'debug';
const statePath = path.join(tauriDir, 'target', 'dev-workers-state.json');
const force = process.argv.includes('--force');

const rootWorkerOutputs = [
  path.join(tauriDir, 'target', profile, 'carbonpaper-ml.exe'),
  path.join(tauriDir, 'target', profile, 'carbonpaper-office.exe'),
  path.join(tauriDir, 'target', profile, 'carbonpaper-nmh.exe'),
];

const semanticOutputs = [
  path.join(tauriDir, 'semantic-worker', 'target', profile, 'carbonpaper-semantic-worker.exe'),
  path.join(tauriDir, 'pre-bundle', 'carbonpaper-semantic-worker.exe'),
  path.join(tauriDir, 'target', profile, 'carbonpaper-semantic-worker.exe'),
];

const rootWorkerPrebundleOutputs = [
  path.join(tauriDir, 'pre-bundle', 'carbonpaper-ml.exe'),
  path.join(tauriDir, 'pre-bundle', 'carbonpaper-office.exe'),
  path.join(tauriDir, 'pre-bundle', 'carbonpaper-nmh.exe'),
];

function walkRustFiles(directory) {
  if (!existsSync(directory)) return [];
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...walkRustFiles(fullPath));
    } else if (entry.isFile() && entry.name.endsWith('.rs')) {
      files.push(fullPath);
    }
  }
  return files;
}

function rootInputFiles() {
  return [
    path.join(tauriDir, 'Cargo.toml'),
    path.join(tauriDir, 'Cargo.lock'),
    path.join(tauriDir, 'build.rs'),
    path.join(tauriDir, 'tauri.conf.json'),
    path.join(tauriDir, 'app.manifest'),
    path.join(tauriDir, 'embedded_manifest.rc'),
    path.join(tauriDir, 'icons', 'icon.ico'),
    path.join(tauriDir, 'update-public-key.txt'),
    path.join(root, 'scripts', 'build-dev-workers.mjs'),
    path.join(tauriDir, 'src', 'bin', 'ml.rs'),
    path.join(tauriDir, 'src', 'ann_format.rs'),
    path.join(tauriDir, 'src', 'ml_protocol.rs'),
    path.join(tauriDir, 'src', 'bin', 'office.rs'),
    path.join(tauriDir, 'src', 'office_com.rs'),
    path.join(tauriDir, 'src', 'office_protocol.rs'),
    path.join(tauriDir, 'src', 'office_window.rs'),
    path.join(tauriDir, 'src', 'bin', 'nmh.rs'),
  ].sort();
}

function semanticInputFiles() {
  return [
    path.join(tauriDir, 'semantic-worker', 'Cargo.toml'),
    path.join(tauriDir, 'semantic-worker', 'Cargo.lock'),
    path.join(root, 'scripts', 'build-semantic-ml.mjs'),
    path.join(root, 'scripts', 'release-assets', 'onnxruntime-directml.json'),
    ...walkRustFiles(path.join(tauriDir, 'semantic-worker', 'src')),
    // The semantic worker imports these files with #[path] attributes.
    path.join(tauriDir, 'src', 'ml_protocol.rs'),
    path.join(tauriDir, 'src', 'clip_preprocess.rs'),
    path.join(tauriDir, 'src', 'semantic_engine.rs'),
    path.join(tauriDir, 'src', 'semantic_models.rs'),
  ].sort();
}

function fingerprint(files) {
  const hash = createHash('sha256');
  for (const file of files) {
    if (!existsSync(file)) {
      throw new Error(`Development worker input is missing: ${file}`);
    }
    hash.update(path.relative(root, file).replace(/\\/g, '/'));
    hash.update('\0');
    hash.update(readFileSync(file));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function hashFile(file) {
  const hash = createHash('sha256');
  hash.update(readFileSync(file));
  return hash.digest('hex');
}

function copyFileIfChanged(source, destination) {
  if (existsSync(destination)) {
    const sourceMetadata = statSync(source);
    const destinationMetadata = statSync(destination);
    if (
      sourceMetadata.size === destinationMetadata.size &&
      hashFile(source) === hashFile(destination)
    ) {
      return false;
    }
  }
  mkdirSync(path.dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  return true;
}

function outputSignature(files) {
  return files.map((file) => {
    if (!existsSync(file)) return null;
    const metadata = statSync(file);
    return {
      path: path.relative(root, file).replace(/\\/g, '/'),
      size: metadata.size,
      sha256: hashFile(file),
    };
  });
}

function outputsReady() {
  return [...rootWorkerOutputs, ...semanticOutputs].every((file) => existsSync(file));
}

function readState() {
  if (!existsSync(statePath)) return null;
  try {
    return JSON.parse(readFileSync(statePath, 'utf8'));
  } catch {
    return null;
  }
}

function writeState(value) {
  mkdirSync(path.dirname(statePath), { recursive: true });
  const temporaryPath = `${statePath}.tmp-${process.pid}`;
  writeFileSync(temporaryPath, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
  renameSync(temporaryPath, statePath);
}

function removeRootWorkerPrebundleCopies() {
  for (const file of rootWorkerPrebundleOutputs) {
    if (!existsSync(file)) continue;
    try {
      rmSync(file, { force: true });
    } catch (error) {
      throw new Error(
        `Cannot remove stale development resource ${file}. ` +
          `Close the running CarbonPaper debug instance and retry. ${error.message}`,
      );
    }
  }
}

function ensureSemanticResourceCopy() {
  const staged = path.join(tauriDir, 'pre-bundle', 'carbonpaper-semantic-worker.exe');
  const target = path.join(tauriDir, 'target', profile, 'carbonpaper-semantic-worker.exe');
  if (!existsSync(staged)) {
    throw new Error(`Staged semantic worker is missing: ${staged}`);
  }
  copyFileIfChanged(staged, target);
}

const currentRootFingerprint = fingerprint(rootInputFiles());
const currentSemanticFingerprint = fingerprint(semanticInputFiles());
const previousState = readState();
const currentOutputs = [...rootWorkerOutputs, ...semanticOutputs];
const currentRootOutputSignature = outputSignature(rootWorkerOutputs);
const currentSemanticOutputSignature = outputSignature(semanticOutputs);
const rootOutputsChanged =
  JSON.stringify(previousState?.rootOutputs) !== JSON.stringify(currentRootOutputSignature);
const semanticOutputsChanged =
  JSON.stringify(previousState?.semanticOutputs) !== JSON.stringify(currentSemanticOutputSignature);

if (
  !force &&
  outputsReady() &&
  previousState?.version === 2 &&
  previousState.rootFingerprint === currentRootFingerprint &&
  previousState.semanticFingerprint === currentSemanticFingerprint &&
  !rootOutputsChanged &&
  !semanticOutputsChanged
) {
  console.log('Development workers are unchanged; skipping Cargo builds.');
  removeRootWorkerPrebundleCopies();
  process.exit(0);
}

const semanticChanged =
  force ||
  !previousState ||
  previousState.version !== 2 ||
  previousState.semanticFingerprint !== currentSemanticFingerprint ||
  semanticOutputsChanged ||
  !semanticOutputs.every((file) => existsSync(file));

if (semanticChanged) {
  console.log('Building development semantic worker...');
  execFileSync('node', [path.join(root, 'scripts', 'build-semantic-ml.mjs')], {
    cwd: root,
    stdio: 'inherit',
  });
} else {
  console.log('Development semantic worker is unchanged; skipping its Cargo build.');
}

// The semantic worker lives in a separate Cargo package, while Tauri loads the
// copy staged in its own target directory during development.
ensureSemanticResourceCopy();

// Semantic worker output is staged independently and does not invalidate the
// ML/Office/NMH binaries; keeping this decision separate is the main startup
// cost saving for changes isolated to semantic search.
const rootChanged =
  force ||
  !previousState ||
  previousState.version !== 2 ||
  previousState.rootFingerprint !== currentRootFingerprint ||
  rootOutputsChanged ||
  !rootWorkerOutputs.every((file) => existsSync(file));

if (rootChanged) {
  console.log('Building development ML, Office, and NMH workers in one Cargo invocation...');
  execFileSync(
    'cargo',
    [
      'build',
      '--manifest-path',
      path.join(tauriDir, 'Cargo.toml'),
      // Match the feature set used by Tauri's `cargo run` dev command. Keeping
      // both paths on the same fingerprint avoids alternating rebuilds between
      // default and no-default feature profiles on every debug startup.
      '--no-default-features',
      '--bin',
      'carbonpaper-ml',
      '--bin',
      'carbonpaper-office',
      '--bin',
      'carbonpaper-nmh',
    ],
    { cwd: root, stdio: 'inherit' },
  );
} else {
  console.log('Development ML, Office, and NMH workers are unchanged; skipping their Cargo build.');
}

for (const file of rootWorkerOutputs) {
  if (!existsSync(file)) {
    throw new Error(`Development worker was not produced: ${file}`);
  }
}
for (const file of semanticOutputs) {
  if (!existsSync(file)) {
    throw new Error(`Development semantic worker output is missing: ${file}`);
  }
}

removeRootWorkerPrebundleCopies();
const finalOutputSignature = outputSignature(currentOutputs);
writeState({
  version: 2,
  rootFingerprint: currentRootFingerprint,
  semanticFingerprint: currentSemanticFingerprint,
  rootOutputs: finalOutputSignature.slice(0, rootWorkerOutputs.length),
  semanticOutputs: finalOutputSignature.slice(rootWorkerOutputs.length),
});
console.log('Development workers are ready.');
