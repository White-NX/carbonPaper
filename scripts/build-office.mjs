// Builds the isolated Office COM worker. Tauri bundles Cargo [[bin]] targets
// for NSIS, while the portable packer reads the binary from target/<profile>.
import { execFileSync } from 'node:child_process';
import { existsSync, rmSync } from 'node:fs';
import path from 'node:path';

const tauriDir = path.join(process.cwd(), 'src-tauri');
const isRelease = process.argv.includes('--release');
const profile = isRelease ? 'release' : 'debug';
const args = ['build', '--bin', 'carbonpaper-office'];
if (isRelease) args.push('--release');

console.log(`Building carbonpaper-office (${profile})...`);
try {
  execFileSync('cargo', args, { cwd: tauriDir, stdio: 'inherit' });
} catch {
  console.error('Failed to build carbonpaper-office');
  process.exit(1);
}

const source = path.join(tauriDir, 'target', profile, 'carbonpaper-office.exe');
if (!existsSync(source)) {
  console.error(`Office worker not found at ${source}`);
  process.exit(1);
}
rmSync(path.join(tauriDir, 'pre-bundle', 'carbonpaper-office.exe'), { force: true });
console.log(`Office worker ready at ${source}`);
