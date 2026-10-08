import { fileURLToPath } from 'node:url';
import { signProtectedRuntime } from './protected-runtime.mjs';

try {
  // makensis runs from its output directory, not the repository root.
  const root = fileURLToPath(new URL('../', import.meta.url));
  const manifest = await signProtectedRuntime(root, process.env.CARBONPAPER_UPDATE_SIGNING_KEY);
  console.log(`Protected runtime ${manifest.version}: signed ${Object.keys(manifest.files).length} files.`);
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
