import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const args = process.argv.slice(2);
const update = args.includes('--update');
const skillRootIndex = args.indexOf('--skill-root');
const skillRoot = skillRootIndex === -1
  ? null
  : resolve(root, args[skillRootIndex + 1]);

if (skillRootIndex !== -1 && !args[skillRootIndex + 1]) {
  throw new Error('--skill-root requires a path');
}

const cargo = spawnSync(
  'cargo',
  [
    'run',
    '--quiet',
    '--manifest-path',
    resolve(root, 'src-tauri/Cargo.toml'),
    '--bin',
    'export_mcp_contract',
  ],
  { cwd: root, encoding: 'utf8' },
);

if (cargo.status !== 0) {
  process.stderr.write(cargo.stderr || 'Failed to export the Rust MCP contract.\n');
  process.exit(cargo.status || 1);
}

const runtimeContract = JSON.parse(cargo.stdout);
const targets = [resolve(root, 'docs/mcp-tool-contract-v2.json')];
if (skillRoot) {
  targets.push(resolve(
    skillRoot,
    'carbonpaper-memory/references/mcp-tool-contract-v2.json',
  ));
}

const stableJson = `${JSON.stringify(runtimeContract, null, 2)}\n`;

for (const target of targets) {
  if (update) {
    writeFileSync(target, stableJson, 'utf8');
    process.stdout.write(`updated ${target}\n`);
    continue;
  }

  let checkedIn;
  try {
    checkedIn = JSON.parse(readFileSync(target, 'utf8'));
  } catch (error) {
    process.stderr.write(`cannot read MCP contract ${target}: ${error.message}\n`);
    process.exitCode = 1;
    continue;
  }

  if (JSON.stringify(checkedIn) !== JSON.stringify(runtimeContract)) {
    process.stderr.write(`MCP contract drift: ${target}\n`);
    process.exitCode = 1;
  } else {
    process.stdout.write(`verified ${target}\n`);
  }
}
