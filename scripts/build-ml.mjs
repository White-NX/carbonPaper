// Builds the isolated Rust ML worker. Tauri bundles Cargo [[bin]] targets for
// NSIS, while the portable packer reads the binary from target/<profile>.
import { execFileSync } from 'node:child_process';
import { existsSync, rmSync } from 'node:fs';
import path from 'node:path';

const tauriDir = path.join(process.cwd(), 'src-tauri');
const isRelease = process.argv.includes('--release');
const profile = isRelease ? 'release' : 'debug';
const args = ['build', '--bin', 'carbonpaper-ml'];
if (isRelease) args.push('--release');

console.log(`Building carbonpaper-ml (${profile})...`);
try {
  execFileSync('cargo', args, { cwd: tauriDir, stdio: 'inherit' });
} catch {
  console.error('Failed to build carbonpaper-ml');
  process.exit(1);
}

const source = path.join(tauriDir, 'target', profile, 'carbonpaper-ml.exe');
if (!existsSync(source)) {
  console.error(`Rust ML worker not found at ${source}`);
  process.exit(1);
}
rmSync(path.join(tauriDir, 'pre-bundle', 'carbonpaper-ml.exe'), { force: true });
console.log(`Rust ML worker ready at ${source}`);
