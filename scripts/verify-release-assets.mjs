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

const buildRs = readText(path.join('src-tauri', 'build.rs'));
assertIncludes(buildRs, 'Path::new("../python-3.12.10-amd64.exe")', 'build.rs Python source path');
assertIncludes(buildRs, 'Path::new("pre-bundle/python-3.12.10-amd64.exe")', 'build.rs Python destination path');
assertIncludes(buildRs, 'Path::new("../aria2c.exe")', 'build.rs aria2 source path');
assertIncludes(buildRs, 'Path::new("pre-bundle/aria2c.exe")', 'build.rs aria2 destination path');
assertIncludes(buildRs, 'cargo:rerun-if-changed=../python-3.12.10-amd64.exe', 'build.rs Python rerun input');
assertIncludes(buildRs, 'cargo:rerun-if-changed=../aria2c.exe', 'build.rs aria2 rerun input');

const tauriConf = JSON.parse(readText(path.join('src-tauri', 'tauri.conf.json')));
assertEqual(tauriConf.bundle?.resources?.['pre-bundle'], '.', 'Tauri pre-bundle resource mapping');

const packPortable = readText(path.join('scripts', 'pack-portable.mjs'));
assertIncludes(packPortable, "const preBundleDir = path.join(tauriDir, 'pre-bundle');", 'portable pre-bundle input');
assertIncludes(packPortable, 'await walkDir(preBundleDir, \'\');', 'portable recursive pre-bundle packaging');

const releaseWorkflow = readText(path.join('.github', 'workflows', 'release.yml'));
assertIncludes(releaseWorkflow, 'npm run prepare:release-assets', 'release workflow asset preparation');
assertBefore(releaseWorkflow, 'npm run prepare:release-assets', 'Build Tauri draft release', 'release workflow order');

const packageJson = JSON.parse(readText('package.json'));
assertIncludes(packageJson.scripts?.['tauri:build'] ?? '', 'npm run prepare:release-assets', 'local tauri:build asset preparation');
assertIncludes(packageJson.scripts?.['prepare:release-assets'] ?? '', 'scripts/prepare-release-assets.ps1', 'prepare script command');

const gitignore = readText('.gitignore');
for (const asset of assets) {
  assertIncludes(gitignore, `/${asset.file}`, `.gitignore ${asset.file}`);
}

const trackedAssets = execFileSync('git', ['ls-files', '--', ...assets.map((asset) => asset.file)], {
  cwd: root,
  encoding: 'utf8',
}).trim();
if (trackedAssets) {
  fail(`Release asset binaries are still tracked:\n${trackedAssets}`);
}

console.log('Release asset build and packaging checks passed.');
