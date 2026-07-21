import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const root = process.cwd();

const assets = [
  {
    name: 'Python 3.12.10 installer',
    file: 'python-3.12.10-amd64.exe',
    sha256: '67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb',
  },
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
  return readFileSync(fullPath).length;
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    fail(`${label}: expected ${expected}, got ${actual}`);
  }
}

function assertIncludes(text, needle, label) {
  if (!text.includes(needle)) {
    fail(`${label}: missing ${needle}`);
  }
}

function assertBefore(text, first, second, label) {
  const firstIndex = text.indexOf(first);
  const secondIndex = text.indexOf(second);
  if (firstIndex === -1 || secondIndex === -1 || firstIndex > secondIndex) {
    fail(`${label}: expected ${first} before ${second}`);
  }
}

for (const asset of assets) {
  const rootHash = sha256(asset.file);
  assertEqual(rootHash, asset.sha256, `${asset.name} root checksum`);

  const bundledPath = path.join('src-tauri', 'pre-bundle', asset.file);
  const bundledHash = sha256(bundledPath);
  assertEqual(bundledHash, asset.sha256, `${asset.name} pre-bundle checksum`);
  console.log(`${asset.name} verified: ${asset.sha256}`);
}

const ocrManifestPath = path.join('scripts', 'release-assets', 'ocr-models.json');
const ocrManifest = JSON.parse(readText(ocrManifestPath));
assertEqual(ocrManifest.model_id, 'ppocrv5-ch-mobile', 'OCR model id');
assertEqual(ocrManifest.revision, 'r1', 'OCR model revision');

const generatedModelPaths = [];
for (const asset of ocrManifest.files) {
  const cachePath = path.join('.release-assets', 'ocr', ocrManifest.directory, asset.name);
  const bundledPath = path.join('src-tauri', 'pre-bundle', ocrManifest.bundle_path, asset.name);
  assertEqual(fileSize(cachePath), asset.size, `${asset.name} cache size`);
  assertEqual(sha256(cachePath), asset.sha256, `${asset.name} cache checksum`);
  assertEqual(fileSize(bundledPath), asset.size, `${asset.name} pre-bundle size`);
  assertEqual(sha256(bundledPath), asset.sha256, `${asset.name} pre-bundle checksum`);
  generatedModelPaths.push(cachePath, bundledPath);
  console.log(`Rust OCR model asset verified: ${asset.name} ${asset.sha256}`);
}
const semanticManifest = JSON.parse(
  readText(path.join('scripts', 'release-assets', 'onnxruntime-directml.json')),
);
assertEqual(semanticManifest.version, '1.24.2', 'semantic ONNX Runtime version');
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
  }
}
const buildRs = readText(path.join('src-tauri', 'build.rs'));
assertIncludes(buildRs, 'Path::new("../python-3.12.10-amd64.exe")', 'build.rs Python source path');
assertIncludes(buildRs, 'Path::new("pre-bundle/python-3.12.10-amd64.exe")', 'build.rs Python destination path');
assertIncludes(buildRs, 'Path::new("../aria2c.exe")', 'build.rs aria2 source path');
assertIncludes(buildRs, 'Path::new("pre-bundle/aria2c.exe")', 'build.rs aria2 destination path');
assertIncludes(buildRs, 'cargo:rerun-if-changed=../python-3.12.10-amd64.exe', 'build.rs Python rerun input');
assertIncludes(buildRs, 'cargo:rerun-if-changed=../aria2c.exe', 'build.rs aria2 rerun input');

const tauriConf = JSON.parse(readText(path.join('src-tauri', 'tauri.conf.json')));
assertEqual(tauriConf.bundle?.resources?.['pre-bundle'], '.', 'Tauri pre-bundle resource mapping');
assertIncludes(tauriConf.build?.beforeBuildCommand ?? '', 'npm run test:release-assets', 'Tauri release asset build gate');

const mlBuild = readText(path.join('scripts', 'build-ml.mjs'));
assertIncludes(mlBuild, "'carbonpaper-ml.exe'", 'ML worker build output');
assertIncludes(mlBuild, "path.join(tauriDir, 'target', profile", 'ML worker target destination');
assertIncludes(mlBuild, "rmSync(path.join(tauriDir, 'pre-bundle', 'carbonpaper-ml.exe')", 'ML worker stale pre-bundle cleanup');

const nmhBuild = readText(path.join('scripts', 'build-nmh.mjs'));
assertIncludes(nmhBuild, "path.join(tauriDir, 'target', profile", 'NMH target destination');
assertIncludes(nmhBuild, "rmSync(path.join(tauriDir, 'pre-bundle', 'carbonpaper-nmh.exe')", 'NMH stale pre-bundle cleanup');

const semanticBuild = readText(path.join('scripts', 'build-semantic-ml.mjs'));
assertIncludes(semanticBuild, "'carbonpaper-semantic-worker.exe'", 'semantic worker build output');
assertIncludes(semanticBuild, "'onnxruntime-directml.json'", 'semantic runtime manifest');

const packPortable = readText(path.join('scripts', 'pack-portable.mjs'));
assertIncludes(packPortable, "const preBundleDir = path.join(tauriDir, 'pre-bundle');", 'portable pre-bundle input');
assertIncludes(packPortable, 'await walkDir(preBundleDir, \'\');', 'portable recursive pre-bundle packaging');
assertIncludes(packPortable, "'carbonpaper-ml.exe'", 'portable required ML worker');
assertIncludes(packPortable, "'carbonpaper-semantic-worker.exe'", 'portable required semantic worker');
assertIncludes(packPortable, 'onnxruntime/1.24.2/onnxruntime.dll', 'portable semantic ONNX runtime');
assertIncludes(packPortable, 'ocr-models/ppocrv5-ch-mobile-r1', 'portable required OCR model directory');

const releaseWorkflow = readText(path.join('.github', 'workflows', 'release.yml'));
assertIncludes(releaseWorkflow, 'npm run prepare:release-assets', 'release workflow asset preparation');
assertIncludes(releaseWorkflow, 'npm run build:ml:release', 'release workflow ML worker build');
assertIncludes(releaseWorkflow, 'npm run build:semantic-ml:release', 'release workflow semantic worker build');
assertIncludes(releaseWorkflow, 'npm run verify:semantic-runtime', 'release workflow semantic smoke test');
assertIncludes(releaseWorkflow, 'npm run test:release-assets', 'release workflow asset verification');
assertBefore(releaseWorkflow, 'npm run prepare:release-assets', 'Build Tauri draft release', 'release workflow order');
assertBefore(releaseWorkflow, 'npm run build:ml:release', 'Build Tauri draft release', 'release workflow ML order');
assertBefore(releaseWorkflow, 'npm run build:semantic-ml:release', 'Build Tauri draft release', 'release workflow semantic ML order');
assertBefore(releaseWorkflow, 'npm run test:release-assets', 'Build Tauri draft release', 'release workflow verification order');

const packageJson = JSON.parse(readText('package.json'));
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run prepare:release-assets', 'local tauri:build asset preparation');
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run build:ml:release', 'local tauri:build ML worker preparation');
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run build:semantic-ml:release', 'local tauri:build semantic worker preparation');
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run verify:semantic-runtime', 'local tauri:build semantic smoke test');
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run verify:ml-runtime', 'local tauri:build ML worker smoke test');
assertIncludes(packageJson.scripts?.['verify:ml-runtime'] ?? '', '--verify-models', 'ML worker verification command');
assertIncludes(packageJson.scripts?.['verify:release-bundles'] ?? '', 'verify-release-bundles.ps1', 'release bundle verification command');
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run verify:release-bundles', 'local bundle verification gate');
assertIncludes(packageJson.scripts?.['prepare:release-assets'] ?? '', 'scripts/prepare-release-assets.ps1', 'prepare script command');
assertIncludes(packageJson.scripts?.['prepare:dev-assets'] ?? '', '-DevelopmentOnly', 'development asset preparation command');
assertIncludes(packageJson.scripts?.['prepare:ocr-assets'] ?? '', '-OcrOnly', 'OCR-only preparation command');
assertIncludes(packageJson.scripts?.debug ?? '', 'npm run prepare:dev-assets', 'debug development asset preparation');
assertBefore(packageJson.scripts?.debug ?? '', 'npm run prepare:dev-assets', 'npm run build:semantic-ml', 'debug semantic asset order');

const prepareAssets = readText(path.join('scripts', 'prepare-release-assets.ps1'));
assertIncludes(prepareAssets, '$includeReleaseTools = -not $OcrOnly -and -not $DevelopmentOnly', 'development asset release-tool exclusion');
assertIncludes(prepareAssets, '$includeSemanticRuntime = -not $OcrOnly', 'development semantic runtime inclusion');

const gitignore = readText('.gitignore');
for (const asset of assets) {
  assertIncludes(gitignore, `/${asset.file}`, `.gitignore ${asset.file}`);
}
assertIncludes(gitignore, '/.release-assets/', '.gitignore release asset cache');

const trackedAssets = execFileSync('git', ['ls-files', '--', ...assets.map((asset) => asset.file)], {
  cwd: root,
  encoding: 'utf8',
}).trim();
if (trackedAssets) {
  fail(`Release asset binaries are still tracked:\n${trackedAssets}`);
}

const trackedGeneratedModels = execFileSync('git', ['ls-files', '--', ...generatedModelPaths], {
  cwd: root,
  encoding: 'utf8',
}).trim();
if (trackedGeneratedModels) {
  fail(`Generated OCR model binaries are still tracked:\n${trackedGeneratedModels}`);
}

console.log('Release asset build and packaging checks passed.');
