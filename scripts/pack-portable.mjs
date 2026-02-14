import { createWriteStream, existsSync } from 'node:fs';
import { readFile, readdir, stat, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { createDeflateRaw } from 'node:zlib';

const cwd = process.cwd();
const tauriDir = path.join(cwd, 'src-tauri');
const releaseDir = path.join(tauriDir, 'target', 'release');
const preBundleDir = path.join(tauriDir, 'pre-bundle');
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

// Simple ZIP creator using Node.js built-ins
// ZIP format: https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT

class ZipWriter {
  constructor(outputPath) {
    this.stream = createWriteStream(outputPath);
    this.entries = [];
    this.offset = 0;
  }

  _write(buf) {
    return new Promise((resolve, reject) => {
      this.stream.write(buf, (err) => err ? reject(err) : resolve());
      this.offset += buf.length;
    });
  }

  async _deflate(data) {
    return new Promise((resolve, reject) => {
      const chunks = [];
      const deflater = createDeflateRaw();
      deflater.on('data', (chunk) => chunks.push(chunk));
      deflater.on('end', () => resolve(Buffer.concat(chunks)));
      deflater.on('error', reject);
      deflater.end(data);
    });
  }

  async addFile(destPath, srcPath) {
    const data = await readFile(srcPath);
    const compressed = await this._deflate(data);
    const crc = crc32(data);
    const fileNameBuf = Buffer.from(destPath.replace(/\\/g, '/'), 'utf-8');
    const fileStat = await stat(srcPath);
    const modDate = toDosDateTime(fileStat.mtime);

    const localHeaderOffset = this.offset;

    // Local file header
    const localHeader = Buffer.alloc(30);
    localHeader.writeUInt32LE(0x04034b50, 0);  // signature
    localHeader.writeUInt16LE(20, 4);           // version needed
    localHeader.writeUInt16LE(0, 6);            // flags
    localHeader.writeUInt16LE(8, 8);            // compression: deflate
    localHeader.writeUInt16LE(modDate.time, 10);
    localHeader.writeUInt16LE(modDate.date, 12);
    localHeader.writeUInt32LE(crc, 14);
    localHeader.writeUInt32LE(compressed.length, 18);
    localHeader.writeUInt32LE(data.length, 22);
    localHeader.writeUInt16LE(fileNameBuf.length, 26);
    localHeader.writeUInt16LE(0, 28);           // extra field length

    await this._write(localHeader);
    await this._write(fileNameBuf);
    await this._write(compressed);

    this.entries.push({
      fileNameBuf,
      crc,
      compressedSize: compressed.length,
      uncompressedSize: data.length,
      localHeaderOffset,
      modDate,
    });
  }

  async finalize() {
    const centralDirOffset = this.offset;

    for (const entry of this.entries) {
      const cdHeader = Buffer.alloc(46);
      cdHeader.writeUInt32LE(0x02014b50, 0);   // signature
      cdHeader.writeUInt16LE(20, 4);            // version made by
      cdHeader.writeUInt16LE(20, 6);            // version needed
      cdHeader.writeUInt16LE(0, 8);             // flags
      cdHeader.writeUInt16LE(8, 10);            // compression: deflate
      cdHeader.writeUInt16LE(entry.modDate.time, 12);
      cdHeader.writeUInt16LE(entry.modDate.date, 14);
      cdHeader.writeUInt32LE(entry.crc, 16);
      cdHeader.writeUInt32LE(entry.compressedSize, 20);
      cdHeader.writeUInt32LE(entry.uncompressedSize, 24);
      cdHeader.writeUInt16LE(entry.fileNameBuf.length, 28);
      cdHeader.writeUInt16LE(0, 30);            // extra field length
      cdHeader.writeUInt16LE(0, 32);            // comment length
      cdHeader.writeUInt16LE(0, 34);            // disk number
      cdHeader.writeUInt16LE(0, 36);            // internal attrs
      cdHeader.writeUInt32LE(0, 38);            // external attrs
      cdHeader.writeUInt32LE(entry.localHeaderOffset, 42);
      await this._write(cdHeader);
      await this._write(entry.fileNameBuf);
    }

    const centralDirSize = this.offset - centralDirOffset;

    // End of central directory
    const eocd = Buffer.alloc(22);
    eocd.writeUInt32LE(0x06054b50, 0);
    eocd.writeUInt16LE(0, 4);                   // disk number
    eocd.writeUInt16LE(0, 6);                   // disk with CD
    eocd.writeUInt16LE(this.entries.length, 8);
    eocd.writeUInt16LE(this.entries.length, 10);
    eocd.writeUInt32LE(centralDirSize, 12);
    eocd.writeUInt32LE(centralDirOffset, 16);
    eocd.writeUInt16LE(0, 20);                  // comment length
    await this._write(eocd);

    return new Promise((resolve) => this.stream.end(resolve));
  }
}

function crc32(buf) {
  let crc = 0xFFFFFFFF;
  for (let i = 0; i < buf.length; i++) {
    crc ^= buf[i];
    for (let j = 0; j < 8; j++) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xEDB88320 : 0);
    }
  }
  return (crc ^ 0xFFFFFFFF) >>> 0;
}

function toDosDateTime(date) {
  return {
    time: (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1),
    date: ((date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
  };
}

// Create output directory if needed
await mkdir(bundleOutDir, { recursive: true });

const zip = new ZipWriter(outPath);
for (const file of filesToPack) {
  console.log(`  Adding: ${file.dest}`);
  await zip.addFile(file.dest, file.src);
}
await zip.finalize();

const outStat = await stat(outPath);
console.log(`\nPortable zip created: ${outPath}`);
console.log(`Size: ${(outStat.size / 1024 / 1024).toFixed(2)} MB`);
