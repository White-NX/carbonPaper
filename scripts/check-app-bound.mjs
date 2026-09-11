import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const root = fileURLToPath(new URL('../', import.meta.url));
const manifest = path.join(root, 'src-tauri', 'app-bound', 'Cargo.toml');
const serviceTest = 'windows::install::tests::existing_service_repair_reapplies_restart_policy';
const requiredTests = [
  serviceTest,
  'windows::transport::tests::delayed_fragmented_reply_is_read_in_full',
  'windows::transport::tests::empty_connected_pipe_waits_until_deadline',
  'windows::service::native_tests::native_policy_round_trip_uses_client_and_server',
  'windows::service::native_tests::native_dpapi_keys_survive_ledger_reopen_and_finish_all_consumers',
  'windows::service::native_tests::native_disable_revokes_keys_and_returns_errors_over_the_pipe',
  'windows::service::native_tests::native_unapproved_process_is_rejected_before_the_request',
  'windows::service::native_tests::native_dpapi_rejects_a_different_pipe_token_sid',
];

function run(program, args, capture = false) {
  const result = spawnSync(program, args, {
    cwd: root,
    windowsHide: true,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', capture ? 'pipe' : 'inherit', 'inherit'],
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${path.basename(program)} failed (${result.signal || result.status})`);
  }
  return result.stdout || '';
}

function testExecutable(output) {
  const executables = new Set();
  for (const line of output.split(/\r?\n/).filter(Boolean)) {
    const message = JSON.parse(line);
    if (message.reason === 'compiler-artifact'
      && message.target?.name === 'carbonpaper_app_bound'
      && message.profile?.test && message.executable) {
      executables.add(message.executable);
    }
  }
  if (executables.size !== 1) throw new Error('Cargo did not produce exactly one app-bound library test executable');
  return [...executables][0];
}

async function main() {
  if (process.platform !== 'win32') throw new Error('The app-bound native check requires Windows');
  if (process.argv.length !== 2) throw new Error('Usage: npm run test:app-bound:fast');
  const started = performance.now();
  const reportDirectory = path.join(root, 'src-tauri', 'app-bound', 'target', 'native-checks',
    `${new Date().toISOString().replace(/[:.]/g, '-')}-${process.pid}`);
  mkdirSync(reportDirectory, { recursive: true });
  const report = { passed: false, started_at: new Date().toISOString(), stages: [] };
  function stage(name, operation) {
    console.log(`\n[app-bound] ${name}`);
    const start = performance.now();
    try {
      const result = operation();
      report.stages.push({ name, passed: true, duration_ms: Math.round(performance.now() - start) });
      return result;
    } catch (error) {
      report.stages.push({ name, passed: false, duration_ms: Math.round(performance.now() - start), error: error.message });
      throw error;
    }
  }
  try {
    const executable = stage('Compile only the app-bound native tests', () => testExecutable(run('cargo', [
      'test', '--locked', '--manifest-path', manifest, '--lib', '--no-run', '--message-format=json-render-diagnostics',
    ], true)));
    stage('Check the native test inventory', () => {
      const listed = new Set(run(executable, ['--list', '--format', 'terse'], true)
        .split(/\r?\n/).filter(line => line.endsWith(': test')).map(line => line.slice(0, -6)));
      for (const name of requiredTests) {
        if (!listed.has(name)) throw new Error(`Required native regression is missing: ${name}`);
      }
    });
    stage('Run protocol, real-pipe, DPAPI and storage tests', () => run(executable, []));
    stage('Check existing-service repair permissions (administrator access)', () => {
      const powershell = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
      run(powershell, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
        path.join(root, 'scripts', 'check-app-bound-service.ps1'),
        '-TestBinary', executable, '-OutputDirectory', reportDirectory]);
      const result = JSON.parse(readFileSync(path.join(reportDirectory, 'service-result.json'), 'utf8').replace(/^\uFEFF/, ''));
      if (result.exit_code !== 0 || result.tests_passed !== 1 || result.test !== serviceTest) {
        throw new Error('The administrator service regression did not complete successfully');
      }
    });
    stage('Check package signatures and privileged imports', () => run(process.execPath, [
      '--test', path.join(root, 'scripts', 'protected-runtime.test.mjs'),
    ]));
    stage('Run security guards', () => run(process.execPath, [path.join(root, 'scripts', 'security-guards.cjs')]));
    report.passed = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    report.duration_ms = Math.round(performance.now() - started);
    writeFileSync(path.join(reportDirectory, 'result.json'), `${JSON.stringify(report, null, 2)}\n`);
    console.log(`\n[app-bound] ${report.passed ? 'PASSED' : 'FAILED'} in ${(report.duration_ms / 1000).toFixed(1)}s`);
    console.log(`[app-bound] Report: ${path.join(reportDirectory, 'result.json')}`);
  }
}

main().catch(error => {
  console.error(`[app-bound] ${error.message}`);
  process.exitCode = 1;
});
