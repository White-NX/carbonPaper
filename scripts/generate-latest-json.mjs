import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';

// Usage: node scripts/generate-latest-json.mjs <tag>
// e.g.   node scripts/generate-latest-json.mjs v0.4.5

const tag = process.argv[2];
if (!tag) {
  console.error('Usage: node scripts/generate-latest-json.mjs <tag>');
  process.exit(1);
}

const version = tag.replace(/^v/, '');
const cwd = process.cwd();
const tauriDir = path.join(cwd, 'src-tauri');
const bundleOutDir = path.join(tauriDir, 'target', 'release', 'bundle', 'nsis');

// Read tauri.conf.json for productName
const tauriConf = JSON.parse(await readFile(path.join(tauriDir, 'tauri.conf.json'), 'utf-8'));
const productName = tauriConf.productName;

const zipFileName = `${productName}_${version}_x64_portable.zip`;
const zipPath = path.join(bundleOutDir, zipFileName);

// Compute SHA256
const zipData = await readFile(zipPath);
const sha256 = createHash('sha256').update(zipData).digest('hex');

const latestJson = {
  version,
  url: `https://github.com/White-NX/carbonPaper/releases/download/${tag}/${zipFileName}`,
  sha256,
  notes: `Release ${tag}`,
  pub_date: new Date().toISOString(),
};

const outPath = path.join(bundleOutDir, 'latest.json');
await writeFile(outPath, JSON.stringify(latestJson, null, 2) + '\n');

console.log(`Generated latest.json:`);
console.log(JSON.stringify(latestJson, null, 2));
console.log(`\nWritten to: ${outPath}`);
