import { beforeEach, describe, expect, it, vi } from 'vitest';
import { open, save } from '@tauri-apps/plugin-dialog';
import { selectBackupExportPath, selectBackupImportPath, selectStorageDirectory } from './native_dialogs';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
beforeEach(() => { open.mockReset(); save.mockReset(); });

describe('native file selection', () => {
  it('keeps export defaults and the backup archive filter', async () => {
    save.mockResolvedValue('D:/Backup.zip');
    await expect(selectBackupExportPath()).resolves.toBe('D:/Backup.zip');
    expect(save).toHaveBeenCalledWith({ defaultPath: 'CarbonPaper_Backup.zip', filters: [{ name: 'CarbonPaper Backup', extensions: ['zip'] }] });
  });

  it('selects exactly one backup file or one storage directory', async () => {
    open.mockResolvedValueOnce(['D:/Backup.zip']).mockResolvedValueOnce('D:/Storage');
    await expect(selectBackupImportPath()).resolves.toBe('D:/Backup.zip');
    expect(open).toHaveBeenNthCalledWith(1, { directory: false, multiple: false, filters: [{ name: 'CarbonPaper Backup', extensions: ['zip'] }] });
    await expect(selectStorageDirectory()).resolves.toBe('D:/Storage');
    expect(open).toHaveBeenNthCalledWith(2, { directory: true, multiple: false });
  });

  it.each([null, undefined, [], ''])('normalizes cancelled or empty selection %j to null', async (selection) => {
    open.mockResolvedValue(selection);
    save.mockResolvedValue(selection);
    await expect(selectBackupExportPath()).resolves.toBeNull();
    await expect(selectBackupImportPath()).resolves.toBeNull();
    await expect(selectStorageDirectory()).resolves.toBeNull();
  });

  it.each([['D:/One', 'D:/Two'], {}, 42])('rejects an invalid single selection %j', async (selection) => {
    open.mockResolvedValue(selection);
    await expect(selectStorageDirectory()).rejects.toThrow('NATIVE_DIALOG_INVALID_RESULT');
  });

  it('preserves native failures so the caller can offer a retry', async () => {
    const failure = new Error('Native picker unavailable');
    open.mockRejectedValue(failure);
    await expect(selectBackupImportPath()).rejects.toBe(failure);
  });
});
