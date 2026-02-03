import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const cwd = process.cwd();
const args = process.argv.slice(2);
const versionArg = args.find((arg) => !arg.startsWith('-'));
const labelArgIndex = args.findIndex((arg) => arg === '--label');
const labelArgValue = labelArgIndex >= 0 ? args[labelArgIndex + 1] : undefined;
const labelArgInline = args.find((arg) => arg.startsWith('--label='));
const dryRun = args.includes('--dry-run');

if (!versionArg) {
  console.error('用法: node scripts/bump-version.mjs <version> [--label "Alpha T.V."] [--dry-run]');
  process.exit(1);
}

const normalizedVersion = versionArg.startsWith('v') ? versionArg.slice(1) : versionArg;
const label = labelArgInline?.slice('--label='.length) ?? labelArgValue ?? 'Alpha T.V.';
const appVersion = label ? `v${normalizedVersion} ${label}` : `v${normalizedVersion}`;

const filePaths = {
  packageJson: path.join(cwd, 'package.json'),
  packageLock: path.join(cwd, 'package-lock.json'),
  tauriConfig: path.join(cwd, 'src-tauri', 'tauri.conf.json'),
  cargoToml: path.join(cwd, 'src-tauri', 'Cargo.toml'),
  appVersionJs: path.join(cwd, 'src', 'lib', 'version.js')
};

const updates = [];

const updateJsonFile = async (filePath, updater) => {
  const raw = await readFile(filePath, 'utf8');
  const data = JSON.parse(raw);
  const updated = updater(data) ?? data;
  const content = JSON.stringify(updated, null, 2) + '\n';
  updates.push({ filePath, content });
};

const updateTextFile = async (filePath, updater) => {
  const raw = await readFile(filePath, 'utf8');
  const content = updater(raw);
  updates.push({ filePath, content });
};

await updateJsonFile(filePaths.packageJson, (data) => {
  data.version = normalizedVersion;
  return data;
});

await updateJsonFile(filePaths.packageLock, (data) => {
  data.version = normalizedVersion;
  if (data.packages && data.packages['']) {
    data.packages[''].version = normalizedVersion;
  }
  return data;
});

await updateJsonFile(filePaths.tauriConfig, (data) => {
  data.version = normalizedVersion;
  return data;
});

await updateTextFile(filePaths.cargoToml, (raw) => {
  const lines = raw.split(/\r?\n/);
  let inPackage = false;
  let updated = false;

  const nextLines = lines.map((line) => {
    const trimmed = line.trim();
    if (trimmed.startsWith('[') && trimmed.endsWith(']')) {
      inPackage = trimmed === '[package]';
    }

    if (inPackage && trimmed.startsWith('version')) {
      updated = true;
      return line.replace(/version\s*=\s*"[^"]+"/, `version = "${normalizedVersion}"`);
    }

    return line;
  });

  if (!updated) {
    throw new Error('未在 Cargo.toml 的 [package] 中找到 version 字段');
  }

  return nextLines.join('\n');
});

await updateTextFile(filePaths.appVersionJs, (raw) => {
  const next = raw.replace(/export const APP_VERSION = ['"][^'"]+['"];?/, `export const APP_VERSION = '${appVersion}';`);
  if (next === raw) {
    throw new Error('未在 src/lib/version.js 中找到 APP_VERSION');
  }
  return next;
});

if (dryRun) {
  console.log('以下文件将被更新:');
  updates.forEach(({ filePath }) => console.log('-', path.relative(cwd, filePath)));
  process.exit(0);
}

await Promise.all(updates.map(({ filePath, content }) => writeFile(filePath, content, 'utf8')));
console.log(`版本已更新为 ${normalizedVersion}`);
