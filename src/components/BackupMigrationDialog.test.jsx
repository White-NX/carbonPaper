import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
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
    fireEvent.change(screen.getByPlaceholderText('Enter Password'), { target: { value: 'review-password' } });
    if (mode === 'export') fireEvent.change(screen.getByPlaceholderText('Confirm Password'), { target: { value: 'review-password' } });
    fireEvent.click(screen.getByRole('button', { name: `settings.backup.${mode}_btn` }));
    await waitFor(() => expect(screen.getByPlaceholderText('Enter Password')).toBeVisible());
    expect(mode === 'export' ? selectBackupExportPath : selectBackupImportPath).toHaveBeenCalledTimes(1);
    expect(withAuth).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });
});
