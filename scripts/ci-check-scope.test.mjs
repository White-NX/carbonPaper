import assert from 'node:assert/strict';
import { test } from 'node:test';
import { needsWindowsChecks } from './ci-check-scope.mjs';

test('UI and prose-only changes can skip Windows checks', () => {
  assert.equal(needsWindowsChecks([
    'src/components/SearchBox.jsx',
    'public/startup.js',
    'docs/ui-overlay-conventions.md',
  ]), false);
});

test('shared build inputs and unclassified paths require Windows checks', () => {
  for (const file of [
    'src/assets/carbonpaper-mark.png',
    'src/i18n/locales/en.json',
    'src/lib/ai_provider_presets.json',
    'src/tests/fixtures/screenshot_contract_v3.json',
    'docs/mcp-tool-contract-v3.json',
    'browser-extension/manifest.json',
    'compliance_process/dicts/terms.txt',
    'rust-toolchain.toml',
    'src-tauri/semantic-worker/Cargo.lock',
    'scripts/check-app-bound.mjs',
    '.github/workflows/backend-tests.yml',
    'unknown-build-input',
  ]) {
    assert.equal(needsWindowsChecks(['src/App.jsx', file]), true, file);
  }
});

test('only dependency fields may change when skipping Windows for an npm update', () => {
  const base = {
    version: '0.9.2',
    scripts: { 'test:app-bound:fast': 'node scripts/check-app-bound.mjs' },
    dependencies: { i18next: '25.8.12' },
  };
  const files = ['package.json', 'package-lock.json'];
  const update = { ...base, dependencies: { i18next: '26.3.4' } };

  assert.equal(needsWindowsChecks(files, base, update), false);
  assert.equal(needsWindowsChecks(files, base, { ...update, version: '0.9.3' }), true);
  assert.equal(needsWindowsChecks(files, base, { ...update, scripts: {} }), true);
  assert.equal(needsWindowsChecks(files, base, undefined), true);
});
