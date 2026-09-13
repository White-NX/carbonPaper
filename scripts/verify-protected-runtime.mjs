import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { verifyProtectedRuntime } from './protected-runtime.mjs';

const directory = process.argv[2];
if (!directory) throw new Error('Usage: node scripts/verify-protected-runtime.mjs <extracted-directory>');
const keyFile = process.argv[3] || fileURLToPath(new URL('../src-tauri/update-public-key.txt', import.meta.url));
const key = readFileSync(keyFile, 'utf8');
await verifyProtectedRuntime(directory, key);
console.log('Protected runtime signature and files verified.');
