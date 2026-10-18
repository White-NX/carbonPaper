// Local semantic-worker probe. Inputs are synthetic; no screenshot database is opened.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
function option(name, fallback) {
  const position = args.indexOf(`--${name}`);
  return position < 0 ? fallback : args[position + 1];
}
const profile = option('profile', 'release');
const samples = Number(option('samples', '20'));
const coldSamples = Number(option('cold-samples', '3'));
const coldTimeoutMs = Number(option('cold-timeout-ms', '20000'));
if (!['debug', 'release'].includes(profile) || !Number.isInteger(samples) || samples < 1
  || !Number.isInteger(coldSamples) || coldSamples < 1
  || !Number.isInteger(coldTimeoutMs) || coldTimeoutMs < 5000) {
  throw new Error('Use --profile debug|release, positive --samples/--cold-samples, and --cold-timeout-ms >= 5000.');
}
const executable = path.resolve(option('worker', path.join(root, 'src-tauri', 'semantic-worker', 'target', profile, 'carbonpaper-semantic-worker.exe')));
const appdata = path.join(process.env.LOCALAPPDATA, 'CarbonPaper');
const models = path.resolve(option('models', path.join(appdata, 'models')));
const ort = path.resolve(option('ort', path.join(root, 'src-tauri', 'target', 'debug', 'onnxruntime', '1.24.2', 'onnxruntime.dll')));
const output = path.resolve(option('output', path.join(root, '.tmp_pytest', 'recap-search', `worker-${profile}.json`)));
for (const file of [executable, ort]) if (!existsSync(file)) throw new Error(`Missing probe input: ${file}`);
const environment = { ...process.env, PATH: `${path.dirname(ort)}${path.delimiter}${process.env.PATH ?? ''}` };
for (const [flag, variable] of [
  ['threads', 'CARBONPAPER_ONNX_INTRA_THREADS'],
  ['optimization', 'CARBONPAPER_ONNX_OPT_LEVEL'],
  ['load-mode', 'CARBONPAPER_ONNX_LOAD_MODE'],
]) {
  const value = option(flag, undefined);
  if (value !== undefined) environment[variable] = value;
}

class Worker {
  constructor() {
    this.frames = [];
    this.waiters = [];
    this.buffer = Buffer.alloc(0);
    this.nextId = 0;
    this.error = null;
    this.stopping = false;
    this.child = spawn(executable, ['--models-root', models, '--onnx-models-root', models,
      '--ort-dylib', ort], { env: environment, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    this.child.stderr.on('data', (chunk) => process.stderr.write(chunk));
    this.child.stdout.on('data', (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      try {
        while (this.buffer.length >= 4) {
          const size = this.buffer.readUInt32LE();
          if (!size || size > 1024 * 1024) throw new Error(`Invalid frame size: ${size}`);
          if (this.buffer.length < size + 4) break;
          const frame = JSON.parse(this.buffer.subarray(4, size + 4).toString('utf8'));
          this.buffer = this.buffer.subarray(size + 4);
          if (this.waiters.length) this.waiters.shift().resolve(frame);
          else this.frames.push(frame);
        }
      } catch (error) { this.fail(error); }
    });
    this.child.once('error', (error) => this.fail(error));
    this.child.stdin.on('error', (error) => this.fail(error));
    this.child.once('exit', (code, signal) => {
      if (!this.stopping) this.fail(new Error(`Worker exited: ${code}, ${signal}`));
    });
  }
  fail(error) {
    this.error ??= error;
    for (const waiter of this.waiters.splice(0)) waiter.reject(error);
  }
  read(timeoutMs = 60_000) {
    if (this.frames.length) return Promise.resolve(this.frames.shift());
    if (this.error) return Promise.reject(this.error);
    return new Promise((resolve, reject) => {
      const waiter = {};
      const timer = setTimeout(() => {
        this.child.kill();
        reject(new Error(`Worker response exceeded ${timeoutMs} ms`));
      }, timeoutMs);
      waiter.resolve = (frame) => { clearTimeout(timer); resolve(frame); };
      waiter.reject = (error) => { clearTimeout(timer); reject(error); };
      this.waiters.push(waiter);
    });
  }
  write(frame) {
    const payload = Buffer.from(JSON.stringify(frame));
    const header = Buffer.alloc(4);
    header.writeUInt32LE(payload.length);
    this.child.stdin.write(Buffer.concat([header, payload]));
  }
  async request(command, fields = {}, budgetMs = 60_000) {
    const requestId = ++this.nextId;
    const started = performance.now();
    this.write({ command, request_id: requestId, ...fields });
    let cancelled = false;
    const cancel = setTimeout(() => {
      cancelled = true;
      this.write({ command: 'cancel', request_id: requestId });
    }, budgetMs);
    try {
      const frame = await this.read(budgetMs + 1_000);
      if (frame.request_id !== requestId) throw new Error(`Unexpected response ID: ${frame.request_id}`);
      return { status: frame.status, error_kind: frame.kind, message: frame.message,
        elapsed_ms: performance.now() - started, cancelled, timings: frame.timings,
        dimensions: frame.dimensions, frame };
    } finally { clearTimeout(cancel); }
  }
  async close() {
    if (this.child.exitCode !== null || this.child.killed || this.error) {
      this.stopping = true;
      this.child.kill();
      return;
    }
    this.stopping = true;
    try { await this.request('shutdown', {}, 5_000); } catch { /* the child is still disposable */ }
    this.child.kill();
  }
}

const query = '\u68c0\u7d22'; // Six UTF-8 bytes, like the reported request.
const chunk = '\u56de\u987e\u641c\u7d22\n' + '\u6bd4\u8f83\u5173\u952e\u8bcd\u4e0e\u8bed\u4e49\u68c0\u7d22\uff0c\u91c7\u7528\u5185\u5b58\u7f13\u5b58\u964d\u4f4e\u67e5\u8be2\u5ef6\u8fdf\u3002'.repeat(2);
const clean = ({ frame, ...result }) => result;
function distribution(rows, key = 'elapsed_ms') {
  const values = rows.map((row) => row[key]).sort((a, b) => a - b);
  const quantile = (q) => values[Math.min(values.length - 1, Math.ceil(values.length * q) - 1)];
  return { samples: values.length, p50_ms: quantile(0.5), p95_ms: quantile(0.95), max_ms: values.at(-1) };
}
const report = { date: new Date().toISOString(), profile, cpu: os.cpus()[0].model,
  logical_processors: os.cpus().length, worker: executable, ort, models,
  configuration: Object.fromEntries(Object.entries(environment).filter(([key]) => key.startsWith('CARBONPAPER_ONNX_'))),
  scope: 'Synthetic local worker requests; excludes database, desktop scheduling and UI.',
  cold_timeout_ms: coldTimeoutMs, cold: [], warm: [], batches: {} };
let worker;
try {
  worker = new Worker();
  const readyStarted = performance.now();
  report.ready = await worker.read();
  report.startup_ms = performance.now() - readyStarted;
  try {
    report.five_second_cold = clean(await worker.request('embed_text',
      { model: 'minilm_l12', texts: [query], timeout_ms: 5_000 }, 5_000));
  } catch (error) {
    report.five_second_cold = { status: 'error', error_kind: 'timeout', message: error.message, cancelled: true };
  }
  console.log(JSON.stringify({ phase: 'five_second_cold', ...report.five_second_cold }));
  if (report.five_second_cold.status !== 'embedding_complete' || worker.error) {
    await worker.close();
    worker = new Worker();
    await worker.read();
  }
  for (let i = 0; i < coldSamples; i++) {
    await worker.request('unload');
    const result = clean(await worker.request('embed_text',
      { model: 'minilm_l12', texts: [query], timeout_ms: coldTimeoutMs }, coldTimeoutMs));
    report.cold.push(result);
    console.log(JSON.stringify({ phase: 'cold', sample: i, ...result }));
    if (result.status !== 'embedding_complete') throw new Error(`Cold inference failed: ${result.message}`);
  }
  for (let i = 0; i < samples; i++) {
    const result = clean(await worker.request('embed_text',
      { model: 'minilm_l12', texts: [query], timeout_ms: 5_000 }, 5_000));
    if (result.status !== 'embedding_complete') throw new Error(`Warm inference failed: ${result.message}`);
    report.warm.push(result);
  }
  for (const batchSize of [4, 8, 16, 32]) {
    const rows = [];
    for (let i = 0; i < Math.min(samples, 5); i++) {
      const result = clean(await worker.request('embed_text',
        { model: 'minilm_l12', texts: Array(batchSize).fill(chunk), timeout_ms: 60_000 }));
      if (result.status !== 'embedding_complete') throw new Error(`Batch inference failed: ${result.message}`);
      rows.push(result);
    }
    report.batches[batchSize] = { input_bytes_per_item: Buffer.byteLength(chunk), rows,
      ...distribution(rows), chunks_per_second_p50: batchSize * 1000 / distribution(rows).p50_ms };
    console.log(JSON.stringify({ phase: 'batch', batch_size: batchSize, ...distribution(rows) }));
  }
  report.cold_distribution = distribution(report.cold);
  report.warm_distribution = distribution(report.warm);
} finally {
  if (worker) await worker.close();
  mkdirSync(path.dirname(output), { recursive: true });
  writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`Report: ${output}`);
}
