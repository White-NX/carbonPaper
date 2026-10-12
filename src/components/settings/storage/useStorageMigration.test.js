import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { runMonitorAction } from '../../../lib/settings_api';
import { withAuth } from '../../../lib/auth_api';
import { useStorageMigration } from './useStorageMigration';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../../../lib/auth_api', () => ({ withAuth: vi.fn((call) => call()) }));
vi.mock('../../../lib/settings_api', () => ({ runMonitorAction: vi.fn(async () => {}), notifySettingsChanged: vi.fn(async () => {}) }));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

let migration;
beforeEach(() => {
  migration = deferred();
  open.mockResolvedValue('D:/NewStorage');
  listen.mockReset().mockImplementation(async () => vi.fn());
  withAuth.mockReset().mockImplementation((call) => call());
  invoke.mockReset().mockImplementation(async (command) => {
    if (command === 'get_monitor_status') return { stopped: false };
    if (command === 'storage_migrate_data_dir') return migration.promise;
    if (command === 'storage_migration_cancel') return { status: 'cancel_requested', in_progress: true };
    return undefined;
  });
});

async function startMigration() {
  const hook = renderHook(() => useStorageMigration({ storage: { root_path: 'C:/OldStorage' }, onRefresh: vi.fn(), t: (key) => key }));
  await act(async () => { await hook.result.current.handleChangeStoragePath(); });
  let operation;
  act(() => { operation = hook.result.current.handleApplyStoragePath(true); });
  await waitFor(() => expect(hook.result.current.isMigrating).toBe(true));
  return { ...hook, operation };
}

describe('storage migration cancellation', () => {
  it('deduplicates cancellation and waits for the migration command to settle', async () => {
    const cancellation = deferred();
    const original = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === 'storage_migration_cancel' ? cancellation.promise : original(command, args));
    const { result, operation } = await startMigration();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_migrate_data_dir', expect.anything()));
    let cancel;
    act(() => { cancel = result.current.handleCancelMigration(); result.current.handleCancelMigration(); });
    expect(invoke.mock.calls.filter(([command]) => command === 'storage_migration_cancel')).toHaveLength(1);
    expect(result.current.migrationCancellation.pending).toBe(true);
    await act(async () => { cancellation.resolve({ status: 'cancel_requested' }); await cancel; });
    expect(result.current.isMigrating).toBe(true);
    expect(result.current.migrationCancellation.requested).toBe(true);
    await act(async () => { migration.reject(new Error('Migration cancelled by user')); await operation; });
    expect(result.current.isMigrating).toBe(false);
    expect(result.current.migrationError).toBe('');
    expect(runMonitorAction).toHaveBeenCalledWith('maintenance-start');
  });

  it('keeps the migration active after cancellation failure and permits retry', async () => {
    const { result, operation } = await startMigration();
    invoke.mockRejectedValueOnce(new Error('AUTH_CANCELLED'));
    await act(async () => { await result.current.handleCancelMigration(); });
    expect(result.current.migrationCancellation).toEqual({ pending: false, requested: false, error: 'AUTH_CANCELLED' });
    expect(result.current.isMigrating).toBe(true);
    await act(async () => { await result.current.handleCancelMigration(); });
    expect(result.current.migrationCancellation.requested).toBe(true);
    await act(async () => { migration.reject(new Error('Migration cancelled by user')); await operation; });
  });

  it('does not claim an idle backend accepted cancellation', async () => {
    const { result, operation } = await startMigration();
    invoke.mockResolvedValueOnce({ status: 'idle', in_progress: false });
    await act(async () => { await result.current.handleCancelMigration(); });
    expect(result.current.migrationCancellation).toEqual({ pending: false, requested: false, error: '' });
    expect(result.current.isMigrating).toBe(true);
    await act(async () => { migration.reject(new Error('Migration cancelled by user')); await operation; });
  });

  it('honors cancellation while progress listeners are still being registered', async () => {
    const registration = deferred();
    listen.mockReturnValueOnce(registration.promise);
    const { result, operation } = await startMigration();
    await act(async () => { await result.current.handleCancelMigration(); });
    await act(async () => { registration.resolve(vi.fn()); await operation; });
    expect(invoke.mock.calls.some(([command]) => command === 'storage_migrate_data_dir')).toBe(false);
    expect(invoke.mock.calls.some(([command]) => command === 'storage_migration_cancel')).toBe(false);
    expect(result.current.isMigrating).toBe(false);
    expect(result.current.migrationError).toBe('');
    expect(runMonitorAction).toHaveBeenCalledWith('maintenance-start');
  });

  it('does not retry migration admission after the user cancels during authentication', async () => {
    const authentication = deferred();
    const original = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === 'storage_migrate_data_dir'
      ? Promise.reject(new Error('AUTH_REQUIRED')) : original(command, args));
    withAuth.mockImplementation(async (call) => {
      try { return await call(); }
      catch (error) {
        if (error.message !== 'AUTH_REQUIRED') throw error;
        await authentication.promise;
        return call();
      }
    });
    const { result, operation } = await startMigration();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_migrate_data_dir', expect.anything()));
    await act(async () => { await result.current.handleCancelMigration(); });
    await act(async () => { authentication.resolve(true); await operation; });
    expect(invoke.mock.calls.filter(([command]) => command === 'storage_migrate_data_dir')).toHaveLength(1);
    expect(result.current.migrationError).toBe('');
    expect(result.current.isMigrating).toBe(false);
  });
});
