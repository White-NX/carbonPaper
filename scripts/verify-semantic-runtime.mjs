import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const exe = path.join(root, 'src-tauri', 'pre-bundle', 'carbonpaper-semantic-worker.exe');
const runtimeDir = path.join(root, 'src-tauri', 'pre-bundle', 'onnxruntime', '1.24.2');
const ort = path.join(runtimeDir, 'onnxruntime.dll');
for (const file of [exe, ort]) {
  if (!existsSync(file)) throw new Error(`Semantic runtime smoke-test file is missing: ${file}`);
}
const expectedWorkerVersion = JSON.parse(
  readFileSync(path.join(root, 'package.json'), 'utf8'),
).version;

// A hung worker must fail the release job quickly instead of idling until the CI
// job-level timeout; every wait below kills the child when its budget expires.
const STARTUP_TIMEOUT_MS = 60_000;
const SHUTDOWN_TIMEOUT_MS = 30_000;

const child = spawn(
  exe,
  [
    '--models-root', path.join(root, '.release-assets', 'semantic-smoke-models'),
    '--onnx-models-root', path.join(root, '.release-assets', 'semantic-smoke-models-onnx'),
    '--ort-dylib', ort,
  ],
  { stdio: ['pipe', 'pipe', 'inherit'] },
);

let buffered = Buffer.alloc(0);
const frames = [];
const waiters = [];
let streamError = null;

function failStream(error) {
  if (streamError) return;
  streamError = error;
  while (waiters.length) waiters.shift().reject(error);
}

child.stdout.on('data', (chunk) => {
  buffered = Buffer.concat([buffered, chunk]);
  try {
    while (buffered.length >= 4) {
      const length = buffered.readUInt32LE(0);
      if (length === 0 || length > 1024 * 1024) {
        throw new Error(`Invalid semantic response frame length: ${length}`);
      }
      if (buffered.length < 4 + length) break;
      const payload = JSON.parse(buffered.subarray(4, 4 + length).toString('utf8'));
      buffered = buffered.subarray(4 + length);
      if (waiters.length) waiters.shift().resolve(payload);
      else frames.push(payload);
    }
  } catch (error) {
    failStream(error);
    child.kill();
  }
});
child.once('error', failStream);
child.once('exit', (code, signal) => {
  failStream(new Error(`Semantic worker exited before the next response: code=${code}, signal=${signal}`));
});

function readFrame(label, timeoutMs) {
  if (frames.length) return Promise.resolve(frames.shift());
  if (streamError) return Promise.reject(streamError);
  return new Promise((resolve, reject) => {
    const waiter = {};
    const timer = setTimeout(() => {
      const index = waiters.indexOf(waiter);
      if (index >= 0) waiters.splice(index, 1);
      child.kill();
      reject(new Error(`Timed out after ${timeoutMs} ms waiting for ${label}`));
    }, timeoutMs);
    waiter.resolve = (value) => { clearTimeout(timer); resolve(value); };
    waiter.reject = (error) => { clearTimeout(timer); reject(error); };
    waiters.push(waiter);
  });
}

function writeFrame(value) {
  const payload = Buffer.from(JSON.stringify(value), 'utf8');
  const header = Buffer.alloc(4);
  header.writeUInt32LE(payload.length);
  child.stdin.write(Buffer.concat([header, payload]));
}

const ready = await readFrame('the semantic worker handshake', STARTUP_TIMEOUT_MS);
const expectedModels = ['bge_reranker_v2_m3', 'bge_small_zh', 'chinese_clip', 'minilm_l12'];
const supportedModels = [...(ready.supported_models ?? [])].sort();
if (
  ready.status !== 'semantic_ready'
  || ready.protocol_version !== 3
  || JSON.stringify(supportedModels) !== JSON.stringify(expectedModels)
) {
  child.kill();
  throw new Error(`Invalid semantic worker handshake: ${JSON.stringify(ready)}`);
}
if (ready.worker_version !== expectedWorkerVersion) {
  child.kill();
  throw new Error(
    `Semantic worker reports version ${ready.worker_version} but package.json is ${expectedWorkerVersion}. `
    + 'Bump src-tauri/semantic-worker/Cargo.toml (npm run bump:version) and rebuild the worker.',
  );
}
writeFrame({ command: 'shutdown', request_id: 1 });
const shutdown = await readFrame('the semantic worker shutdown response', SHUTDOWN_TIMEOUT_MS);
if (shutdown.status !== 'shutting_down' || shutdown.request_id !== 1) {
  child.kill();
  throw new Error(`Invalid semantic worker shutdown: ${JSON.stringify(shutdown)}`);
}
if (child.exitCode === null) {
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`Timed out after ${SHUTDOWN_TIMEOUT_MS} ms waiting for the semantic worker to exit`));
    }, SHUTDOWN_TIMEOUT_MS);
    child.once('exit', (code) => {
      clearTimeout(timer);
      if (code === 0) resolve();
      else reject(new Error(`Exit ${code}`));
    });
  });
} else if (child.exitCode !== 0) {
  throw new Error(`Exit ${child.exitCode}`);
}
console.log(`Semantic runtime ready: ORT ${ready.ort_version}, provider=${ready.provider}, worker=${ready.worker_version}`);
