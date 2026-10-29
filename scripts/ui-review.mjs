import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const mock = path.join(root, 'tools/ui-review/tauri-mock.js');
const port = Number(process.env.UI_REVIEW_PORT || 1426);
const server = await createServer({
  root,
  resolve: { alias: [
    ...['core', 'event', 'window', 'webviewWindow'].map((name) => ({ find: `@tauri-apps/api/${name}`, replacement: mock })),
    ...['opener', 'dialog'].map((name) => ({ find: `@tauri-apps/plugin-${name}`, replacement: mock })),
  ] },
  server: { host: '127.0.0.1', port, strictPort: true },
});
await server.listen();
console.log(`CarbonPaper synthetic UI review: http://127.0.0.1:${port}/tools/ui-review/?page=search`);
console.log('Pages: records, search, recap, topics, preview, standalone, lock. Options: lang=en, theme=light.');
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit(0); });
