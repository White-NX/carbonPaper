import { execFileSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';
import { isDeepStrictEqual } from 'node:util';

const frontendOrDocumentationFiles = new Set([
  'CONTRIBUTING.md',
  'README.en.md',
  'README.md',
  'SECURITY.md',
  'eslint.config.js',
  'index.html',
  'package-lock.json',
  'postcss.config.js',
  'tailwind.config.js',
  'tsconfig.json',
  'tsconfig.node.json',
  'vite.config.ts',
]);

// The desktop build and Rust tests read files from these frontend paths.
const sharedFrontendPaths = [
  'src/assets',
  'src/i18n/locales',
  'src/lib/ai_provider_presets.json',
  'src/tests/fixtures',
];

function canSkipWindowsForPath(file) {
  const lowerCaseFile = file.toLowerCase();
  if (sharedFrontendPaths.some(path => lowerCaseFile === path || lowerCaseFile.startsWith(`${path}/`))) {
    return false;
  }

  return file.startsWith('src/') || file.startsWith('public/') ||
    frontendOrDocumentationFiles.has(file) || (file.startsWith('docs/') && file.endsWith('.md'));
}

function packageSettings(manifest) {
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) return undefined;

  const settings = { ...manifest };
  delete settings.dependencies;
  delete settings.devDependencies;
  return settings;
}

export function needsWindowsChecks(files, baseManifest, mergedManifest) {
  if (files.some(file => file !== 'package.json' && !canSkipWindowsForPath(file))) return true;
  if (!files.includes('package.json')) return false;

  const baseSettings = packageSettings(baseManifest);
  const mergedSettings = packageSettings(mergedManifest);
  return !baseSettings || !mergedSettings || !isDeepStrictEqual(baseSettings, mergedSettings);
}

function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

function classifyPullRequest() {
  // Compare the tree being tested with the base snapshot it merged into.
  git('rev-parse', '--verify', 'HEAD^2');
  const files = git('diff', '--no-renames', '--name-only', '-z', 'HEAD^1', 'HEAD', '--')
    .split('\0').filter(Boolean);
  if (!files.includes('package.json')) return needsWindowsChecks(files);

  const baseManifest = JSON.parse(git('show', 'HEAD^1:package.json'));
  const mergedManifest = JSON.parse(git('show', 'HEAD:package.json'));
  return needsWindowsChecks(files, baseManifest, mergedManifest);
}

if (import.meta.main) {
  let windows = true;
  try {
    windows = classifyPullRequest();
  } catch {
    console.warn('::warning::Could not classify the pull request diff. Running Windows checks.');
  }

  console.log(`Run Windows checks: ${windows}`);
  appendFileSync(process.env.GITHUB_OUTPUT, `windows=${windows}\n`);
}
