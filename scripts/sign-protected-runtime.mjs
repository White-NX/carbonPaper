import { signProtectedRuntime } from './protected-runtime.mjs';

try {
  const manifest = await signProtectedRuntime(process.cwd(), process.env.CARBONPAPER_UPDATE_SIGNING_KEY);
  console.log(`Protected runtime ${manifest.version}: signed ${Object.keys(manifest.files).length} files.`);
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
