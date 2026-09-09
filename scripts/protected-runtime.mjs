import { createHash, createPrivateKey, createPublicKey, sign, verify } from 'node:crypto';
import { closeSync, createReadStream, existsSync, fstatSync, lstatSync, openSync, readFileSync, readSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { verifyPrivilegedImports } from './privileged-imports.mjs';

export const manifestName = 'protected-runtime.json';
export const signatureName = 'protected-runtime.sig';
export const signingContext = Buffer.from('CarbonPaper protected runtime v1\n');
const required = ['carbonpaper.exe', 'carbonpaper-python.exe', 'carbonpaper-key-service.exe',
  'carbonpaper-protected-setup.exe', 'carbonpaper-semantic-worker.exe', 'carbonpaper-ml.exe',
  'carbonpaper-office.exe', 'carbonpaper-nmh.exe', 'monitor.pyz'];
const maxFileBytes = 2 * 1024 ** 3;
const releaseVersion = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*))*)?(?:\+[0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*)?$/;

export function safeRuntimePath(name) {
  if (typeof name !== 'string' || !name || name.length > 240 || /[^\x20-\x7e]|[\\:<>"|?*]/.test(name) || name.startsWith('/')) throw new Error('Unsafe runtime path');
  for (const part of name.split('/')) {
    const stem = part.split('.')[0].toUpperCase();
    if (!part || part === '.' || part === '..' || /[. ]$/.test(part) || /[\x00-\x1f]/.test(part)
      || /^(CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[0-9]|LPT[0-9])$/.test(stem)) throw new Error('Unsafe runtime path');
  }
  return name;
}

async function hashFile(file) {
  const info = lstatSync(file);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error('Runtime entry is not a regular file');
  if (info.size > maxFileBytes) throw new Error('Oversized runtime file');
  const hash = createHash('sha256');
  let size = 0;
  for await (const chunk of createReadStream(file)) {
    size += chunk.length;
    if (size > maxFileBytes) throw new Error('Oversized runtime file');
    hash.update(chunk);
  }
  return hash.digest('hex');
}

function assertNoLinks(directory, relative) {
  let current = directory;
  for (const part of ['', ...relative.split('/')]) {
    current = path.join(current, part);
    if (lstatSync(current).isSymbolicLink()) throw new Error('Runtime resources contain a symbolic link');
  }
}

function readBounded(file, limit) {
  const fd = openSync(file, 'r');
  try {
    const info = fstatSync(fd);
    if (!info.isFile() || info.size > limit) throw new Error('Oversized or invalid runtime metadata');
    const bytes = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = readSync(fd, bytes, length, bytes.length - length, null);
      if (!count) break;
      length += count;
    }
    if (length > limit) throw new Error('Oversized runtime metadata');
    return bytes.subarray(0, length);
  } finally { closeSync(fd); }
}

function validateManifest(manifest) {
  if (manifest.format !== 1 || manifest.product !== 'carbonpaper' || manifest.architecture !== 'x86_64'
    || manifest.service_protocol !== 1 || typeof manifest.version !== 'string'
    || manifest.version.length > 83 || !releaseVersion.test(manifest.version)
    || !manifest.files || Array.isArray(manifest.files) || typeof manifest.files !== 'object'
    || Object.keys(manifest.files).length > 4096) throw new Error('Unsupported protected runtime');
  const names = new Set();
  for (const name of required) if (!Object.hasOwn(manifest.files, name)) throw new Error(`Protected runtime is missing ${name}`);
  for (const [name, hash] of Object.entries(manifest.files)) {
    safeRuntimePath(name);
    const folded = name.toLowerCase();
    if (names.has(folded)) throw new Error('Duplicate Windows runtime path');
    names.add(folded);
    if (typeof hash !== 'string' || !/^[0-9a-f]{64}$/.test(hash)) throw new Error('Invalid runtime checksum');
  }
}

function walk(directory, prefix = '') {
  if (lstatSync(directory).isSymbolicLink()) throw new Error('Runtime resources contain a symbolic link');
  const entries = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) throw new Error('Runtime resources contain a symbolic link');
    const name = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) entries.push(...walk(path.join(directory, entry.name), name));
    else if (entry.isFile() && ![manifestName, signatureName].includes(name)) entries.push([safeRuntimePath(name), path.join(directory, entry.name)]);
  }
  return entries;
}

function publicKey(raw) {
  const bytes = Buffer.from(raw.trim(), 'base64');
  if (bytes.length !== 32) throw new Error('Invalid release public key');
  return createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), bytes]), format: 'der', type: 'spki' });
}

export async function signProtectedRuntime(root, signingKeyBase64) {
  if (!signingKeyBase64) throw new Error('CARBONPAPER_UPDATE_SIGNING_KEY is required to package the protected runtime');
  const tauri = path.join(root, 'src-tauri');
  const key = createPrivateKey({ key: Buffer.from(signingKeyBase64, 'base64'), format: 'pem' });
  const trustedKey = readFileSync(path.join(tauri, 'update-public-key.txt'), 'utf8').trim();
  const actualKey = createPublicKey(key).export({ format: 'der', type: 'spki' }).subarray(-32).toString('base64');
  if (actualKey !== trustedKey) throw new Error('Signing key does not match the application release public key');
  const config = JSON.parse(readFileSync(path.join(tauri, 'tauri.conf.json'), 'utf8'));
  const entries = new Map(walk(path.join(tauri, 'pre-bundle')));
  for (const name of ['carbonpaper.exe', 'carbonpaper-ml.exe', 'carbonpaper-office.exe', 'carbonpaper-nmh.exe', 'carbonpaper-python.exe']) {
    if (entries.has(name)) throw new Error(`Duplicate bundled executable: ${name}`);
    entries.set(name, path.join(tauri, 'target', 'release', name));
  }
  for (const name of required) if (!entries.has(name) || !existsSync(entries.get(name))) throw new Error(`Protected runtime is missing ${name}`);
  for (const name of ['carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe']) verifyPrivilegedImports(entries.get(name));
  const files = {};
  for (const name of [...entries.keys()].sort()) {
    assertNoLinks(root, path.relative(root, entries.get(name)).replaceAll('\\', '/'));
    files[name] = await hashFile(entries.get(name));
  }
  const manifest = { format: 1, product: 'carbonpaper', version: config.version, architecture: 'x86_64', service_protocol: 1, files };
  validateManifest(manifest);
  const bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  if (bytes.length > 1024 * 1024) throw new Error('Oversized runtime manifest');
  const signature = sign(null, Buffer.concat([signingContext, bytes]), key).toString('base64');
  writeFileSync(path.join(tauri, 'pre-bundle', manifestName), bytes);
  writeFileSync(path.join(tauri, 'pre-bundle', signatureName), `${signature}\n`);
  return manifest;
}

export async function verifyProtectedRuntime(directory, rawPublicKey) {
  assertNoLinks(directory, manifestName);
  assertNoLinks(directory, signatureName);
  const bytes = readBounded(path.join(directory, manifestName), 1024 * 1024);
  const signature = Buffer.from(readBounded(path.join(directory, signatureName), 256).toString('utf8').trim(), 'base64');
  if (!verify(null, Buffer.concat([signingContext, bytes]), publicKey(rawPublicKey), signature)) throw new Error('Invalid protected runtime signature');
  const manifest = JSON.parse(bytes);
  validateManifest(manifest);
  for (const [relative, expected] of Object.entries(manifest.files)) {
    assertNoLinks(directory, relative);
    const actual = await hashFile(path.join(directory, relative));
    if (actual !== expected) throw new Error(`Protected runtime checksum mismatch: ${relative}`);
  }
  for (const name of ['carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe']) verifyPrivilegedImports(path.join(directory, name));
  return manifest;
}
