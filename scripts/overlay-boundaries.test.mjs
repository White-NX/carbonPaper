import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ESLint } from 'eslint';

const eslint = new ESLint();
const ordinaryView = 'src/components/settings/BoundaryProbe.jsx';

async function violations(source, filePath = ordinaryView) {
  const [result] = await eslint.lintText(source, { filePath });
  assert.equal(result.fatalErrorCount, 0);
  return result.messages.filter(({ ruleId }) => ruleId?.startsWith('no-restricted-'));
}

test('native file selection rejects direct, aliased, re-exported and dynamic plugin access', async () => {
  for (const source of [
    "import { open as select } from '@tauri-apps/plugin-dialog';",
    "import * as dialogs from '@tauri-apps/plugin-dialog';",
    "export { save } from '@tauri-apps/plugin-dialog';",
    "export * from '@tauri-apps/plugin-dialog';",
    "const dialogs = await import('@tauri-apps/plugin-dialog');",
    "const dialogs = require('@tauri-apps/plugin-dialog');",
  ]) {
    assert.equal((await violations(source)).length, 1, source);
  }
});

test('native gateway and test mocks retain direct access', async () => {
  const source = "import { open, save } from '@tauri-apps/plugin-dialog';";
  assert.deepEqual(await violations(source, 'src/lib/native_dialogs.js'), []);
  assert.deepEqual(await violations(source, 'src/lib/native_dialogs.test.js'), []);
  assert.deepEqual(await violations("import { selectStorageDirectory } from '../../lib/native_dialogs';"), []);
});

test('focus-stack ownership stays in the shared primitives and fatal surface', async () => {
  const source = "import { useDialogFocus as focus } from '../hooks/useDialogFocus';";
  assert.equal((await violations(source)).length, 1);
  assert.equal((await violations("import * as focus from '../hooks/useDialogFocus';")).length, 1);
  for (const filePath of [
    'src/components/Dialog.jsx', 'src/components/ConfirmDialog.jsx',
    'src/components/overlay/OverlayShell.jsx', 'src/components/ErrorWindow.jsx',
  ]) {
    assert.deepEqual(await violations(source, filePath), []);
  }
  assert.deepEqual(await violations("import { useDialogVisibility, DialogVisibilityContext } from '../hooks/useDialogFocus';"), []);
});

test('overlay restrictions preserve translation checks and permit ordinary positioning', async () => {
  for (const filePath of [ordinaryView, 'src/components/Dialog.jsx', 'src/lib/native_dialogs.js']) {
    const messages = await violations("t('dialog.title', 'Inline default');", filePath);
    assert.equal(messages.length, 1);
    assert.match(messages[0].message, /translation defaults/);
  }
  assert.deepEqual(await violations('export const Menu = () => <div className="fixed z-50" />;'), []);
});
