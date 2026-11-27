import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertCachePath, assertNoRustBuilds, DEFAULT_CACHE_MAX_AGE_DAYS, removeCacheDirectory, rustCleanupPlan } from './rust-cache.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));

function main() {
  const args = process.argv.slice(2);
  let apply = false;
  let allDev = false;
  let maxAgeDays = DEFAULT_CACHE_MAX_AGE_DAYS;
  for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === '--apply') apply = true;
    else if (argument === '--all-dev') allDev = true;
    else if (argument === '--max-age-days') {
      const value = args[++index];
      if (!value || !/^\d+$/.test(value) || !Number.isSafeInteger(Number(value))) {
        throw new Error('--max-age-days requires a nonnegative integer');
      }
      maxAgeDays = Number(value);
    } else if (argument === '--help') {
      console.log('Usage: npm run clean:rust -- [--apply] [--all-dev] [--max-age-days DAYS]');
      console.log('Defaults to a preview. Cleans local packages and incremental caches; expires unused development instances after 7 days.');
      console.log('--all-dev resets development dependency caches too, while retaining release artifacts.');
      return;
    } else throw new Error(`Unknown argument: ${argument}`);
  }
  const plan = rustCleanupPlan(root, { allDev, maxAgeDays });
  console.log(`${apply ? 'Cleaning' : 'Previewing'} Rust caches; unused development instance retention: ${maxAgeDays} day(s).`);
  if (apply) assertNoRustBuilds();
  for (const args of plan.cargo) {
    if (apply) assertNoRustBuilds();
    const target = args[args.indexOf('--target-dir') + 1];
    assertCachePath(root, target, { allowRoot: true });
    const command = apply ? args : [...args, '--dry-run'];
    console.log(`cargo ${command.map(argument => argument.includes(' ') ? `"${argument}"` : argument).join(' ')}`);
    const result = spawnSync('cargo', command, { cwd: root, stdio: 'inherit', windowsHide: true });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`cargo clean failed (${result.status ?? result.signal})`);
  }
  if (apply && plan.directories.length) assertNoRustBuilds();
  for (const { directory, reason } of plan.directories) {
    if (apply && !existsSync(directory)) continue;
    console.log(`${apply ? 'Removing' : 'Would remove'} ${path.relative(root, directory)} (${reason})`);
    if (apply) removeCacheDirectory(root, directory);
  }
  if (!apply) console.log('Preview complete. Add --apply to delete the listed caches.');
}

try { main(); }
catch (error) { console.error(error.message); process.exitCode = 1; }
