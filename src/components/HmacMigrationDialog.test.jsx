import React from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import HmacMigrationDialog from './HmacMigrationDialog';

const events = vi.hoisted(() => new Map());
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name, handler) => { events.set(name, handler); return vi.fn(); }),
}));
let status;
beforeEach(() => {
  events.clear();
  status = { needs_migration: true, is_running: true, processed: 512, total: 1000, error: null, revision: 2 };
  invoke.mockReset().mockImplementation(async (command) => {
    if (command === 'storage_check_hmac_migration_status') return status;
    throw new Error('Unexpected command: ' + command);
  });
});
afterEach(() => vi.useRealTimers());

describe('application-owned search-index upgrade', () => {
  it('recovers a running job on mount and recreation without starting another upgrade', async () => {
    const view = render(<HmacMigrationDialog />);
    await screen.findByRole('dialog');
    await userEvent.click(screen.getByRole('button', { name: 'common.close' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    view.unmount();
    render(<HmacMigrationDialog />);
    await screen.findByRole('dialog');
    expect(invoke.mock.calls.every(([command]) => command === 'storage_check_hmac_migration_status')).toBe(true);
  });

  it('waits for native admission without requesting authentication or running migration', async () => {
    status = { ...status, is_running: false };
    render(<HmacMigrationDialog />);
    await act(async () => {});
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    act(() => events.get('hmac-migration-progress')({ payload: { ...status, is_running: true, revision: 3 } }));
    await screen.findByRole('dialog');
    expect(invoke.mock.calls.every(([command]) => command === 'storage_check_hmac_migration_status')).toBe(true);
  });

  it('keeps newer progress when an old mount query arrives late', async () => {
    invoke.mockReset();
    let finishRead;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }));
    render(<HmacMigrationDialog />);
    await act(async () => {});
    act(() => events.get('hmac-migration-progress')({ payload: status }));
    await screen.findByRole('dialog');
    await act(async () => finishRead({ ...status, needs_migration: false, is_running: false, revision: 1 }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('shows a retained native failure without retrying it from the view', async () => {
    status = { ...status, is_running: false, error: 'disk failure' };
    render(<HmacMigrationDialog />);
    await screen.findByText('disk failure');
    expect(invoke.mock.calls.every(([command]) => command === 'storage_check_hmac_migration_status')).toBe(true);
  });
});
