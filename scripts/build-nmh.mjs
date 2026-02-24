// scripts/build-nmh.mjs
// Builds the carbonpaper-nmh binary and copies it to pre-bundle/
// Called before tauri build to ensure the NMH host is included in the bundle.

import { execSync } from 'node:child_process';
import { existsSync, mkdirSync, copyFileSync } from 'node:fs';
import path from 'node:path';

const tauriDir = path.join(process.cwd(), 'src-tauri');
const isRelease = process.argv.includes('--release');
const profile = isRelease ? 'release' : 'debug';

console.log(`Building carbonpaper-nmh (${profile})...`);

try {
  const args = ['cargo', 'build', '--bin', 'carbonpaper-nmh'];
  if (isRelease) args.push('--release');
  execSync(args.join(' '), { cwd: tauriDir, stdio: 'inherit' });
} catch (e) {
  console.error('Failed to build carbonpaper-nmh');
  process.exit(1);
}

const src = path.join(tauriDir, 'target', profile, 'carbonpaper-nmh.exe');
const destDir = path.join(tauriDir, 'pre-bundle');
const dest = path.join(destDir, 'carbonpaper-nmh.exe');

if (!existsSync(src)) {
  console.error(`NMH binary not found at ${src}`);
  process.exit(1);
}

mkdirSync(destDir, { recursive: true });
copyFileSync(src, dest);
console.log(`Copied carbonpaper-nmh.exe to ${dest}`);
