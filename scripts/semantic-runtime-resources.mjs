import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

export function verifySemanticRuntimeResources(root, resourceDirectory) {
  const manifest = JSON.parse(readFileSync(
    path.join(root, 'scripts', 'release-assets', 'onnxruntime-directml.json'), 'utf8',
  ));
  const runtimeDirectory = path.join(resourceDirectory, ...manifest.bundle_path.split('/'));
  const files = manifest.packages.flatMap((pkg) => pkg.files).map((file) => ({
    ...file,
    path: path.join(runtimeDirectory, file.name),
  }));
  for (const file of files) {
    if (!existsSync(file.path) || !statSync(file.path).isFile()) {
      throw new Error(`Pinned semantic runtime resource is missing: ${file.path}`);
    }
    if (statSync(file.path).size !== file.size || sha256(file.path) !== file.sha256) {
      throw new Error(`Pinned semantic runtime resource checksum mismatch: ${file.path}`);
    }
  }
  return files;
}

export function stageDevelopmentSemanticRuntime(root, profile = 'debug') {
  const stagedDirectory = path.join(root, 'src-tauri', 'pre-bundle');
  const resourceDirectory = path.join(root, 'src-tauri', 'target', profile);
  const files = verifySemanticRuntimeResources(root, stagedDirectory);
  const outputs = [];
  for (const file of files) {
    const destination = path.join(resourceDirectory, path.relative(stagedDirectory, file.path));
    if (
      !existsSync(destination)
      || !statSync(destination).isFile()
      || statSync(destination).size !== file.size
      || sha256(destination) !== file.sha256
    ) {
      mkdirSync(path.dirname(destination), { recursive: true });
      copyFileSync(file.path, destination);
    }
    outputs.push(file.path, destination);
  }
  return outputs;
}
