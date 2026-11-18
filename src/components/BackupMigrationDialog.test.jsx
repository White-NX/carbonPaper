import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { withAuth } from '../lib/auth_api';
import { selectBackupExportPath, selectBackupImportPath } from '../lib/native_dialogs';
import BackupMigrationDialog from './BackupMigrationDialog';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('../lib/auth_api', () => ({ withAuth: vi.fn((call) => call()) }));
vi.mock('../lib/native_dialogs', () => ({ selectBackupExportPath: vi.fn(async () => null), selectBackupImportPath: vi.fn(async () => null) }));

describe('backup selection cancellation', () => {
  it.each(['export', 'import'])('keeps the %s form open without authenticating or starting work', async (mode) => {
    render(<BackupMigrationDialog isOpen mode={mode} onClose={vi.fn()} />);
    fireEvent.change(screen.getByPlaceholderText('settings.backup.password_placeholder'), { target: { value: 'review-password' } });
    if (mode === 'export') fireEvent.change(screen.getByPlaceholderText('settings.backup.confirm_password_placeholder'), { target: { value: 'review-password' } });
    fireEvent.click(screen.getByRole('button', { name: `settings.backup.${mode}_btn` }));
    await waitFor(() => expect(screen.getByPlaceholderText('settings.backup.password_placeholder')).toBeVisible());
    expect(mode === 'export' ? selectBackupExportPath : selectBackupImportPath).toHaveBeenCalledTimes(1);
    expect(withAuth).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });
});

it('shows counted preparation progress and reports omitted images on success', async () => {
  let progress;
  let complete;
  listen.mockImplementation(async (name, handler) => {
    if (name === 'backup-migration-progress') progress = handler;
    return vi.fn();
  });
  selectBackupExportPath.mockResolvedValue('D:/backup.cpbackup');
  invoke.mockImplementation(() => new Promise(resolve => { complete = resolve; }));
  render(<BackupMigrationDialog isOpen mode="export" onClose={vi.fn()} />);
  fireEvent.change(screen.getByPlaceholderText('settings.backup.password_placeholder'), { target: { value: 'review-password' } });
  fireEvent.change(screen.getByPlaceholderText('settings.backup.confirm_password_placeholder'), { target: { value: 'review-password' } });
  fireEvent.click(screen.getByRole('button', { name: 'settings.backup.export_btn' }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_export_backup', expect.anything()));
  act(() => progress({ payload: { phase: 'snapshot', copied_files: 64, total_files: 256 } }));
  expect(screen.getByRole('progressbar', { name: 'settings.backup.phases.snapshot' })).toHaveAttribute('aria-valuenow', '25');
  act(() => progress({ payload: { phase: 'images', copied_files: 200, total_files: 1000 } }));
  expect(screen.getByRole('progressbar', { name: 'settings.backup.phases.images' })).toHaveAttribute('aria-valuenow', '20');
  await act(async () => complete({ missing_screenshots: 11, repaired_paths: 3 }));
  expect(screen.getByText('settings.backup.export_missing_hint')).toBeVisible();
});
