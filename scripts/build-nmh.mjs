// scripts/build-nmh.mjs
// Builds the carbonpaper-nmh binary. Tauri bundles Cargo [[bin]] targets for
// NSIS, while the portable packer reads the binary from target/<profile>.

import { execSync } from 'node:child_process';
import { existsSync, rmSync } from 'node:fs';
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

if (!existsSync(src)) {
  console.error(`NMH binary not found at ${src}`);
  process.exit(1);
}

rmSync(path.join(tauriDir, 'pre-bundle', 'carbonpaper-nmh.exe'), { force: true });
console.log(`NMH binary ready at ${src}`);
