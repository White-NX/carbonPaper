import { createPrivateKey, createPublicKey, generateKeyPairSync, sign } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';

// Signs an update manifest in place and prints the matching raw Ed25519 public
// key (base64) on stdout. Uses CARBONPAPER_UPDATE_SIGNING_KEY (base64 PKCS#8
// PEM, same format as generate-latest-json.mjs) when available, otherwise an
// ephemeral key pair so local smoke runs can still exercise verification.
const manifestPath = process.argv[2];
if (!manifestPath) {
  console.error('Usage: node scripts/sign-update-manifest.mjs <manifest.json>');
  process.exit(1);
}

const signingKeyBase64 = process.env.CARBONPAPER_UPDATE_SIGNING_KEY;
const privateKey = signingKeyBase64
  ? createPrivateKey({ key: Buffer.from(signingKeyBase64, 'base64'), format: 'pem' })
  : generateKeyPairSync('ed25519').privateKey;

const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
const signingPayload = [
  manifest.version,
  manifest.url,
  String(manifest.sha256).toLowerCase(),
  manifest.critical ? '1' : '0',
  manifest.min_version || '',
].join('\n');
manifest.signature = sign(null, Buffer.from(signingPayload, 'utf8'), privateKey).toString('base64');
await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + '\n');

const publicKeyDer = createPublicKey(privateKey).export({ format: 'der', type: 'spki' });
console.log(publicKeyDer.subarray(publicKeyDer.length - 32).toString('base64'));
