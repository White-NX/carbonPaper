import { existsSync } from 'node:fs';
import { readFile, readdir, stat, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { ZipWriter } from './zip-writer.mjs';
import { signProtectedRuntime } from './protected-runtime.mjs';
import { packServiceRepair } from './service-repair.mjs';

const cwd = process.cwd();
const tauriDir = path.join(cwd, 'src-tauri');
const releaseDir = path.join(tauriDir, 'target', 'release');
const preBundleDir = path.join(tauriDir, 'pre-bundle');
// Re-sign the final binaries, including forward-update smoke-test builds.
await signProtectedRuntime(cwd, process.env.CARBONPAPER_UPDATE_SIGNING_KEY);
const bundleOutDir = path.join(releaseDir, 'bundle', 'nsis');

// Read version from tauri.conf.json
const tauriConf = JSON.parse(await readFile(path.join(tauriDir, 'tauri.conf.json'), 'utf-8'));
const version = tauriConf.version;
const productName = tauriConf.productName;

const outFileName = `${productName}_${version}_x64_portable.zip`;
const outPath = path.join(bundleOutDir, outFileName);

console.log(`Creating portable zip: ${outFileName}`);

// Collect files to include: same as what NSIS packages
const filesToPack = [];

// 1. Main binary
const mainExe = path.join(releaseDir, `${productName}.exe`);
if (!existsSync(mainExe)) {
  console.error(`Main binary not found: ${mainExe}`);
  process.exit(1);
}
filesToPack.push({ src: mainExe, dest: `${productName}.exe` });

for (const binaryName of ['carbonpaper-ml.exe', 'carbonpaper-office.exe', 'carbonpaper-nmh.exe']) {
  const binaryPath = path.join(releaseDir, binaryName);
  if (!existsSync(binaryPath)) {
    console.error(`Required portable binary not found: ${binaryPath}`);
    process.exit(1);
  }
  filesToPack.push({ src: binaryPath, dest: binaryName });
}

// 2. All pre-bundle resources
async function walkDir(dir, prefix) {
  const entries = await readdir(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    const destPath = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      await walkDir(fullPath, destPath);
    } else {
      filesToPack.push({ src: fullPath, dest: destPath });
    }
  }
}

if (existsSync(preBundleDir)) {
  await walkDir(preBundleDir, '');
}

const requiredPortableEntries = [
  'carbonpaper-ml.exe',
  'carbonpaper-office.exe',
  'carbonpaper-nmh.exe',
  'carbonpaper-semantic-worker.exe',
  'onnxruntime/1.24.2/onnxruntime.dll',
  'onnxruntime/1.24.2/onnxruntime_providers_shared.dll',
  'onnxruntime/1.24.2/DirectML.dll',
  'ocr-models/ppocrv5-ch-mobile-r1/ch_PP-OCRv5_det_mobile.onnx',
  'ocr-models/ppocrv5-ch-mobile-r1/ch_PP-OCRv5_rec_mobile.onnx',
  'ocr-models/ppocrv5-ch-mobile-r1/ppocrv5_dict.txt',
];
const packedDestinations = new Set(filesToPack.map(({ dest }) => dest.replace(/\\/g, '/')));
for (const requiredEntry of requiredPortableEntries) {
  if (!packedDestinations.has(requiredEntry)) {
    console.error(`Required portable runtime resource not found: ${requiredEntry}`);
    process.exit(1);
  }
}
// Create output directory if needed
await mkdir(bundleOutDir, { recursive: true });

const zip = new ZipWriter(outPath);
try {
  for (const file of filesToPack) {
    console.log(`  Adding: ${file.dest}`);
    await zip.addFile(file.dest, file.src);
  }
  await zip.finalize();
} finally {
  await zip.abort();
}

const repair = await packServiceRepair(filesToPack, bundleOutDir,
  await readFile(path.join(tauriDir, 'update-public-key.txt'), 'utf8'), process.env.CARBONPAPER_UPDATE_SIGNING_KEY);
console.log(`Service repair zip created: ${repair.zipPath}`);

const outStat = await stat(outPath);
console.log(`\nPortable zip created: ${outPath}`);
console.log(`Size: ${(outStat.size / 1024 / 1024).toFixed(2)} MB`);
