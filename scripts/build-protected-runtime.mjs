import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { verifyPrivilegedImports } from './privileged-imports.mjs';

const release = process.argv.includes('--release');
const profile = release ? 'release' : 'debug';
const root = process.cwd();
const tauri = path.join(root, 'src-tauri');
const flags = release ? ['--release'] : [];
execFileSync('cargo', ['build', '--manifest-path', path.join(tauri, 'Cargo.toml'), '--bin', 'carbonpaper-python', ...flags], { stdio: 'inherit' });
execFileSync('cargo', ['build', '--manifest-path', path.join(tauri, 'app-bound', 'Cargo.toml'), '--bins', ...flags], {
  stdio: 'inherit',
  // The UAC bootstrap starts in an unprotected release folder. Link its CRT
  // statically so it cannot side-load a user-supplied VC runtime before main.
  env: { ...process.env, RUSTFLAGS: `${process.env.RUSTFLAGS || ''} -C target-feature=+crt-static`.trim() },
});
const bundle = path.join(tauri, 'pre-bundle');
mkdirSync(bundle, { recursive: true });
for (const binary of ['carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe']) {
  const built = path.join(tauri, 'app-bound', 'target', profile, binary);
  verifyPrivilegedImports(built);
  copyFileSync(built, path.join(bundle, binary));
}
console.log(`Protected runtime components built (${profile}).`);
