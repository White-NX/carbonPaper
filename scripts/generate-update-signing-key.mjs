import { generateKeyPairSync } from 'node:crypto';
import { writeFile } from 'node:fs/promises';

const { privateKey, publicKey } = generateKeyPairSync('ed25519');
const privatePem = privateKey.export({ format: 'pem', type: 'pkcs8' });
const publicDer = publicKey.export({ format: 'der', type: 'spki' });
const publicRaw = publicDer.subarray(publicDer.length - 32);

console.log(`CARBONPAPER_UPDATE_SIGNING_KEY=${Buffer.from(privatePem).toString('base64')}`);
console.log(`CARBONPAPER_UPDATE_PUBLIC_KEY=${publicRaw.toString('base64')}`);
await writeFile('src-tauri/update-public-key.txt', `${publicRaw.toString('base64')}\n`, { flag: 'wx' });
console.error('Stored the public key in src-tauri/update-public-key.txt. Store only CARBONPAPER_UPDATE_SIGNING_KEY as a GitHub Actions secret; never commit the private key.');
