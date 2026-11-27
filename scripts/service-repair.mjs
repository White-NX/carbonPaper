import { createHash, createPrivateKey, createPublicKey, randomUUID, sign, verify } from 'node:crypto';
import { lstatSync, readFileSync } from 'node:fs';
import { mkdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { manifestName, signatureName, readProtectedManifest } from './protected-runtime.mjs';
import { verifyPrivilegedImports } from './privileged-imports.mjs';
import { ZipWriter } from './zip-writer.mjs';

export const componentFiles = ['carbonpaper-key-service.exe', 'carbonpaper-protected-setup.exe'];
export const repairEntries = [...componentFiles, manifestName, signatureName];
export const maxComponentBytes = 64 * 1024 ** 2;
export const maxExpandedBytes = 128 * 1024 ** 2;
export const releaseRoot = 'https://github.com/White-NX/carbonPaper/releases/download';
const signingContext = 'CarbonPaper service repair v1\n';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

export function repairSigningPayload(release) {
  return signingContext + [release.format, release.product, release.architecture, release.version,
    release.service_protocol, release.runtime_id, release.url, release.sha256, release.size_bytes].join('\n');
}

export function verifyRepairRelease(release, expectedVersion, rawPublicKey) {
  if (release.format !== 1 || release.product !== 'carbonpaper' || release.architecture !== 'x86_64'
    || release.service_protocol !== 1 || release.version !== expectedVersion
    || !new RegExp(`^${release.version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}-[0-9a-f]{16}$`).test(release.runtime_id)
    || release.url !== `${releaseRoot}/v${release.version}/carbonpaper_${release.version}_x64_service-repair.zip`
    || !/^[0-9a-f]{64}$/.test(release.sha256) || !Number.isSafeInteger(release.size_bytes)
    || release.size_bytes <= 0 || release.size_bytes > maxComponentBytes) throw new Error('Unsupported service repair release');
  const bytes = Buffer.from(rawPublicKey.trim(), 'base64');
  if (bytes.length !== 32) throw new Error('Invalid release public key');
  const key = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), bytes]), format: 'der', type: 'spki' });
  if (!verify(null, Buffer.from(repairSigningPayload(release)), key, Buffer.from(release.signature, 'base64'))) {
    throw new Error('Invalid service repair signature');
  }
  return release;
}

export function verifyServiceRepair(directory, publicKey) {
  const release = readProtectedManifest(directory, publicKey);
  let size = 0;
  for (const name of componentFiles) {
    const file = path.join(directory, name);
    const info = lstatSync(file);
    if (!info.isFile() || info.isSymbolicLink()) throw new Error('Invalid repair component');
    size += info.size;
    if (size > maxExpandedBytes) throw new Error('Oversized service repair package');
    if (hash(readFileSync(file)) !== release.files[name]) throw new Error(`Repair checksum mismatch: ${name}`);
    verifyPrivilegedImports(file);
  }
  return release;
}

export function verifyServiceRepairBundle(directory, archivePath, metadataPath, publicKey, expectedVersion) {
  const metadataInfo = lstatSync(metadataPath);
  if (!metadataInfo.isFile() || metadataInfo.isSymbolicLink() || metadataInfo.size > 64 * 1024) {
    throw new Error('Invalid service repair metadata');
  }
  const release = verifyRepairRelease(JSON.parse(readFileSync(metadataPath, 'utf8')), expectedVersion, publicKey);
  const archiveInfo = lstatSync(archivePath);
  if (!archiveInfo.isFile() || archiveInfo.isSymbolicLink() || archiveInfo.size !== release.size_bytes
    || hash(readFileSync(archivePath)) !== release.sha256) throw new Error('Service repair ZIP checksum or size mismatch');
  const runtime = verifyServiceRepair(directory, publicKey);
  const runtimeId = `${runtime.version}-${hash(readFileSync(path.join(directory, manifestName))).slice(0, 16)}`;
  if (runtime.version !== expectedVersion || runtimeId !== release.runtime_id) {
    throw new Error('Service repair runtime identity mismatch');
  }
  return release;
}

export async function packServiceRepair(files, outputDirectory, publicKey, signingKeyBase64) {
  const entries = new Map(files.map(entry => [entry.dest.replaceAll('\\', '/'), entry.src]));
  for (const name of repairEntries) if (!entries.has(name)) throw new Error(`Missing repair entry: ${name}`);
  const metadataDirectory = path.dirname(entries.get(manifestName));
  if (path.dirname(entries.get(signatureName)) !== metadataDirectory) throw new Error('Separated runtime metadata');
  const runtime = readProtectedManifest(metadataDirectory, publicKey);
  let expanded = 0;
  const checksums = new Map();
  for (const name of repairEntries) {
    const file = entries.get(name);
    const info = lstatSync(file);
    if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Invalid repair input: ${name}`);
    expanded += info.size;
    if (expanded > maxExpandedBytes) throw new Error('Oversized service repair inputs');
    const checksum = hash(readFileSync(file));
    if (componentFiles.includes(name)) {
      if (checksum !== runtime.files[name]) throw new Error(`Repair input checksum mismatch: ${name}`);
      verifyPrivilegedImports(file);
    }
    checksums.set(name, checksum);
  }
  const key = createPrivateKey({ key: Buffer.from(signingKeyBase64 || '', 'base64'), format: 'pem' });
  const actualPublic = createPublicKey(key).export({ format: 'der', type: 'spki' }).subarray(-32).toString('base64');
  if (actualPublic !== publicKey.trim()) throw new Error('Repair signing key does not match release public key');
  await mkdir(outputDirectory, { recursive: true });
  const fileName = `carbonpaper_${runtime.version}_x64_service-repair.zip`;
  const zipPath = path.join(outputDirectory, fileName);
  const temporaryZip = `${zipPath}.tmp-${randomUUID()}`;
  const metadataPath = path.join(outputDirectory, 'service-repair.json');
  const temporaryMetadata = `${metadataPath}.tmp-${randomUUID()}`;
  const writer = new ZipWriter(temporaryZip);
  try {
    for (const name of repairEntries) await writer.addFile(name, entries.get(name), checksums.get(name));
    await writer.finalize();
    const size = (await stat(temporaryZip)).size;
    if (size > maxComponentBytes) throw new Error('Service repair ZIP exceeds 64 MiB');
    const release = {
      format: 1, product: 'carbonpaper', architecture: 'x86_64', version: runtime.version,
      service_protocol: runtime.service_protocol,
      runtime_id: `${runtime.version}-${checksums.get(manifestName).slice(0, 16)}`,
      url: `${releaseRoot}/v${runtime.version}/${fileName}`, sha256: hash(readFileSync(temporaryZip)),
      size_bytes: size,
    };
    release.signature = sign(null, Buffer.from(repairSigningPayload(release)), key).toString('base64');
    verifyRepairRelease(release, runtime.version, publicKey);
    await writeFile(temporaryMetadata, `${JSON.stringify(release, null, 2)}\n`);
    await rename(temporaryZip, zipPath);
    await rename(temporaryMetadata, metadataPath);
    return { zipPath, release };
  } finally {
    await writer.abort();
    await rm(temporaryZip, { force: true });
    await rm(temporaryMetadata, { force: true });
  }
}
