import { open, save } from '@tauri-apps/plugin-dialog';

const backupFilters = [{ name: 'CarbonPaper Backup', extensions: ['zip'] }];

function singlePath(selection) {
  if (Array.isArray(selection)) {
    if (selection.length > 1) throw new Error('NATIVE_DIALOG_INVALID_RESULT');
    [selection] = selection;
  }
  if (selection == null || selection === '') return null;
  if (typeof selection !== 'string') throw new Error('NATIVE_DIALOG_INVALID_RESULT');
  return selection;
}

// The dialog plugin attaches native UI to the calling WebView. These helpers
// select paths only; callers authenticate the protected operation afterwards.
export async function selectBackupExportPath() {
  return singlePath(await save({ filters: backupFilters, defaultPath: 'CarbonPaper_Backup.zip' }));
}

export async function selectBackupImportPath() {
  return singlePath(await open({ filters: backupFilters, multiple: false, directory: false }));
}

export async function selectStorageDirectory() {
  return singlePath(await open({ multiple: false, directory: true }));
}
