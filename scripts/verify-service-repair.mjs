import { readFileSync } from 'node:fs';
import { verifyServiceRepairBundle } from './service-repair.mjs';

const [directory, archivePath, metadataPath, publicKeyPath, version] = process.argv.slice(2);
if (process.argv.length !== 7) {
  throw new Error('Usage: node scripts/verify-service-repair.mjs <directory> <zip> <metadata> <public-key> <version>');
}
verifyServiceRepairBundle(directory, archivePath, metadataPath, readFileSync(publicKeyPath, 'utf8'), version);
console.log('Service repair signatures, archive, components and runtime identity verified.');
