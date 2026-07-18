import { readFile, writeFile } from 'node:fs/promises';
import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';
import path from 'node:path';
import { existsSync } from 'node:fs';
import { execSync } from 'node:child_process';

// Usage: node scripts/generate-latest-json.mjs <tag> [body_file]
// e.g.   node scripts/generate-latest-json.mjs v0.4.5 release_body.txt

const tag = process.argv[2];
const bodyFile = process.argv[3];

if (!tag) {
  console.error('Usage: node scripts/generate-latest-json.mjs <tag> [body_file]');
  process.exit(1);
}

const version = tag.replace(/^v/, '');
const cwd = process.cwd();
const tauriDir = path.join(cwd, 'src-tauri');
const bundleOutDir = path.join(tauriDir, 'target', 'release', 'bundle', 'nsis');

// Read tauri.conf.json for productName
const tauriConf = JSON.parse(await readFile(path.join(tauriDir, 'tauri.conf.json'), 'utf-8'));
const productName = tauriConf.productName || 'carbonpaper';

const zipFileName = `${productName}_${version}_x64_portable.zip`;
const zipPath = path.join(bundleOutDir, zipFileName);

// Compute SHA256
const zipData = await readFile(zipPath);
const sha256 = createHash('sha256').update(zipData).digest('hex');

const signingKeyBase64 = process.env.CARBONPAPER_UPDATE_SIGNING_KEY;
if (!signingKeyBase64) {
  throw new Error('CARBONPAPER_UPDATE_SIGNING_KEY is required to sign latest.json');
}

// Parse release body if provided
let notes = `Release ${tag}`;
let critical = false;
let min_version = undefined;

if (bodyFile && existsSync(bodyFile)) {
  try {
    const content = await readFile(bodyFile, 'utf-8');
    if (content && content.trim()) {
      notes = content.trim();
    }
  } catch (err) {
    console.warn(`Failed to read release body file ${bodyFile}:`, err);
  }
}

// 1. Check if CURRENT release is explicitly marked as critical
if (notes.includes('[CRITICAL]') || notes.includes('[critical]')) {
  critical = true;
  min_version = version; // This release becomes the new baseline
} else {
  // 2. If not critical, find the most recent critical release via GitHub CLI
  try {
    // Requires GITHUB_TOKEN to be set in the environment
    console.log('Fetching previous releases to determine min_version...');
    const output = execSync('gh release list --limit 30 --json tagName,body', { encoding: 'utf-8' });
    const releases = JSON.parse(output);
    
    // releases are usually sorted newest first
    for (const rel of releases) {
      // Skip the current release being drafted if it happens to be in the list
      if (rel.tagName === tag) continue;

      if (rel.body && (rel.body.includes('[CRITICAL]') || rel.body.includes('[critical]'))) {
        min_version = rel.tagName.replace(/^v/, '');
        console.log(`Found previous critical baseline: ${min_version} (from ${rel.tagName})`);
        break; // Found the most recent critical baseline
      }
    }
  } catch (e) {
    console.warn('Failed to fetch previous releases for min_version calculation:', e.message);
  }
}

const latestJson = {
  version,
  url: `https://github.com/White-NX/carbonPaper/releases/download/${tag}/${zipFileName}`,
  sha256,
  notes,
  pub_date: new Date().toISOString(),
  critical,
  update_smoke_supported: true,
};

if (min_version) {
  latestJson.min_version = min_version;
}

const signingPayload = [
  latestJson.version,
  latestJson.url,
  latestJson.sha256.toLowerCase(),
  latestJson.critical ? '1' : '0',
  latestJson.min_version || '',
].join('\n');
const signingKey = createPrivateKey({
  key: Buffer.from(signingKeyBase64, 'base64'),
  format: 'pem',
});
const publicKeyFile = process.env.CARBONPAPER_UPDATE_PUBLIC_KEY_FILE || 'src-tauri/update-public-key.txt';
const configuredPublicKey = (await readFile(path.join(cwd, publicKeyFile), 'utf8')).trim();
const publicKeyDer = createPublicKey(signingKey).export({ format: 'der', type: 'spki' });
const derivedPublicKey = publicKeyDer.subarray(publicKeyDer.length - 32).toString('base64');
if (derivedPublicKey !== configuredPublicKey) {
  throw new Error('CARBONPAPER_UPDATE_SIGNING_KEY does not match CARBONPAPER_UPDATE_PUBLIC_KEY');
}
latestJson.signature = sign(null, Buffer.from(signingPayload, 'utf8'), signingKey).toString('base64');

const outPath = path.join(bundleOutDir, 'latest.json');
await writeFile(outPath, JSON.stringify(latestJson, null, 2) + '\n');

console.log(`Generated latest.json:`);
console.log(JSON.stringify(latestJson, null, 2));
console.log(`\nWritten to: ${outPath}`);
