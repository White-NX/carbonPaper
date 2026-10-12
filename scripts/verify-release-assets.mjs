import { createHash } from 'node:crypto';
import { existsSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const root = process.cwd();

const assets = [
  {
    name: 'aria2 1.37.0 Windows x64',
    file: 'aria2c.exe',
    sha256: 'be2099c214f63a3cb4954b09a0becd6e2e34660b886d4c898d260febfe9d70c2',
  },
];

function fail(message) {
  console.error(`ERROR: ${message}`);
  process.exit(1);
}

function readText(relPath) {
  return readFileSync(path.join(root, relPath), 'utf8');
}

function sha256(relPath) {
  const fullPath = path.join(root, relPath);
  if (!existsSync(fullPath)) {
    fail(`Missing ${relPath}`);
  }
  return createHash('sha256')
    .update(readFileSync(fullPath))
    .digest('hex');
}

function fileSize(relPath) {
  const fullPath = path.join(root, relPath);
  if (!existsSync(fullPath)) {
    fail(`Missing ${relPath}`);
  }
  return statSync(fullPath).size;
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    fail(`${label}: expected ${expected}, got ${actual}`);
  }
}

const generatedPaths = [];
for (const asset of assets) {
  const rootHash = sha256(asset.file);
  assertEqual(rootHash, asset.sha256, `${asset.name} root checksum`);

  const bundledPath = path.join('src-tauri', 'pre-bundle', asset.file);
  const bundledHash = sha256(bundledPath);
  assertEqual(bundledHash, asset.sha256, `${asset.name} pre-bundle checksum`);
  generatedPaths.push(asset.file, bundledPath);
  console.log(`${asset.name} verified: ${asset.sha256}`);
}

const ocrManifestPath = path.join('scripts', 'release-assets', 'ocr-models.json');
const ocrManifest = JSON.parse(readText(ocrManifestPath));
for (const asset of ocrManifest.files) {
  const cachePath = path.join('.release-assets', 'ocr', ocrManifest.directory, asset.name);
  const bundledPath = path.join('src-tauri', 'pre-bundle', ocrManifest.bundle_path, asset.name);
  assertEqual(fileSize(cachePath), asset.size, `${asset.name} cache size`);
  assertEqual(sha256(cachePath), asset.sha256, `${asset.name} cache checksum`);
  assertEqual(fileSize(bundledPath), asset.size, `${asset.name} pre-bundle size`);
  assertEqual(sha256(bundledPath), asset.sha256, `${asset.name} pre-bundle checksum`);
  generatedPaths.push(cachePath, bundledPath);
  console.log(`Rust OCR model asset verified: ${asset.name} ${asset.sha256}`);
}
const semanticManifest = JSON.parse(
  readText(path.join('scripts', 'release-assets', 'onnxruntime-directml.json')),
);
for (const pkg of semanticManifest.packages) {
  for (const asset of pkg.files) {
    const cachePath = path.join('.release-assets', 'onnxruntime', semanticManifest.version, asset.name);
    const bundledPath = path.join(
      'src-tauri',
      'pre-bundle',
      semanticManifest.bundle_path,
      asset.name,
    );
    assertEqual(fileSize(cachePath), asset.size, `${asset.name} semantic cache size`);
    assertEqual(sha256(cachePath), asset.sha256, `${asset.name} semantic cache checksum`);
    assertEqual(fileSize(bundledPath), asset.size, `${asset.name} semantic pre-bundle size`);
    assertEqual(sha256(bundledPath), asset.sha256, `${asset.name} semantic pre-bundle checksum`);
    generatedPaths.push(cachePath, bundledPath);
  }
}

const tauriConf = JSON.parse(readText(path.join('src-tauri', 'tauri.conf.json')));
assertEqual(tauriConf.bundle?.resources?.['pre-bundle'], '.', 'Tauri pre-bundle resource mapping');
// Check Git's effective rules, including nested ignore files, rather than the
// spelling of .gitignore. Build ordering and package contents are exercised by
// the release workflow and verify:release-bundles against the actual artifacts.
const trackedAssets = execFileSync('git', ['ls-files', '--', ...generatedPaths], {
  cwd: root,
  encoding: 'utf8',
}).trim();
if (trackedAssets) {
  fail(`Release asset binaries are still tracked:\n${trackedAssets}`);
}

const ignoredPaths = new Set(execFileSync('git', ['check-ignore', '--stdin'], {
  cwd: root,
  encoding: 'utf8',
  input: generatedPaths.map((file) => file.replace(/\\/g, '/')).join('\n') + '\n',
}).trim().split(/\r?\n/));
for (const file of generatedPaths) {
  if (!ignoredPaths.has(file.replace(/\\/g, '/'))) {
    fail(`Generated asset is not ignored by Git: ${file}`);
  }
}

console.log('Release asset checks passed. Run verify:release-bundles for packaged artifacts.');
