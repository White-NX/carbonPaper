import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { selectBackupExportPath, selectBackupImportPath } from '../lib/native_dialogs';
import { useBackupMigration } from './useBackupMigration';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('../lib/auth_api', () => ({ withAuth: vi.fn((call) => call()) }));
vi.mock('../lib/native_dialogs', () => ({ selectBackupExportPath: vi.fn(), selectBackupImportPath: vi.fn() }));
vi.mock('../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}) }));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

let backup;
beforeEach(() => {
  backup = deferred();
  selectBackupExportPath.mockReset().mockResolvedValue('D:/backup.cpbackup');
  selectBackupImportPath.mockReset().mockResolvedValue('D:/legacy.zip');
  withAuth.mockReset().mockImplementation((call) => call());
  invoke.mockReset().mockImplementation((command) => {
    if (command === 'storage_backup_cancel') return Promise.resolve(true);
    return backup.promise;
  });
});

async function startBackup(mode = 'export') {
  const hook = renderHook(() => useBackupMigration({ mode, t: (key) => key, onClose: vi.fn() }));
  act(() => {
    hook.result.current.setPassword('backup-password');
    hook.result.current.setConfirmPassword('backup-password');
  });
  let operation;
  act(() => { operation = hook.result.current.handleAction(); });
  await waitFor(() => expect(hook.result.current.status).toBe('processing'));
  return { ...hook, operation };
}

describe('backup cancellation', () => {
  it('prevents admission after cancelling while the native picker is open', async () => {
    const selection = deferred();
    selectBackupExportPath.mockReturnValueOnce(selection.promise);
    const { result, operation } = await startBackup();
    await act(async () => { await result.current.handleCancel(); });
    await act(async () => { selection.resolve('D:/backup.cpbackup'); await operation; });
    expect(invoke).not.toHaveBeenCalled();
    expect(withAuth).not.toHaveBeenCalled();
    expect(result.current.status).toBe('idle');
  });

  it('deduplicates accepted cancellation and keeps progress open until the worker settles', async () => {
    const cancellation = deferred();
    const { result, operation } = await startBackup();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_export_backup', expect.anything()));
    invoke.mockReturnValueOnce(cancellation.promise);
    let cancel;
    act(() => { cancel = result.current.handleCancel(); result.current.handleCancel(); });
    expect(invoke.mock.calls.filter(([command]) => command === 'storage_backup_cancel')).toHaveLength(1);
    await act(async () => { cancellation.resolve(true); await cancel; });
    expect(result.current.status).toBe('processing');
    expect(result.current.cancelState).toBe('pending');
    await act(async () => { backup.reject('BACKUP_CANCELLED'); await operation; });
    expect(result.current.status).toBe('idle');
    expect(result.current.error).toBe('');
  });

  it('prevents an authentication retry from starting work after cancellation', async () => {
    const authentication = deferred();
    invoke.mockRejectedValue(new Error('AUTH_REQUIRED'));
    withAuth.mockImplementation(async (call) => {
      try { return await call(); }
      catch (error) {
        if (error.message !== 'AUTH_REQUIRED') throw error;
        await authentication.promise;
        return call();
      }
    });
    const { result, operation } = await startBackup();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_export_backup', expect.anything()));
    await act(async () => { await result.current.handleCancel(); });
    await act(async () => { authentication.resolve(true); await operation; });
    expect(invoke.mock.calls.filter(([command]) => command === 'storage_export_backup')).toHaveLength(1);
    expect(invoke.mock.calls.some(([command]) => command === 'storage_backup_cancel')).toBe(false);
    expect(result.current.status).toBe('idle');
    expect(result.current.error).toBe('');
  });

  it('keeps a failed cancellation retryable without hiding ongoing work', async () => {
    const { result, operation } = await startBackup();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_export_backup', expect.anything()));
    invoke.mockRejectedValueOnce(new Error('AUTH_CANCELLED'));
    await act(async () => { await result.current.handleCancel(); });
    expect(result.current.status).toBe('processing');
    expect(result.current.cancelState).toBe('idle');
    expect(result.current.error).toBe('AUTH_CANCELLED');
    await act(async () => { await result.current.handleCancel(); });
    expect(result.current.cancelState).toBe('pending');
    await act(async () => { backup.reject('BACKUP_CANCELLED'); await operation; });
  });

  it('waits for a commit already in progress when cancellation is rejected', async () => {
    const { result, operation } = await startBackup();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_export_backup', expect.anything()));
    invoke.mockResolvedValueOnce(false);
    await act(async () => { await result.current.handleCancel(); });
    expect(result.current.status).toBe('processing');
    expect(result.current.cancelState).toBe('finishing');
    await act(async () => { backup.resolve(); await operation; });
    expect(result.current.status).toBe('success');
  });
});

it.each([
  ['BACKUP_LEGACY_DEVICE_REQUIRED', 'settings.backup.error_legacy_device'],
  ['BACKUP_INCORRECT_PASSWORD_OR_CORRUPT', 'settings.backup.error_password_or_corrupt'],
])('translates %s into a recovery explanation', async (failure, translation) => {
  const { result, operation } = await startBackup('import');
  await act(async () => { backup.reject(failure); await operation; });
  expect(result.current.status).toBe('error');
  expect(result.current.error).toBe(translation);
});
