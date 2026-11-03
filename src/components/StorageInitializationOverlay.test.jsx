import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import StorageInitializationOverlay from './StorageInitializationOverlay';
import { pickOverlay } from './overlay/coordinator';
import { useStorageInitialization } from '../hooks/useStorageInitialization';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

const initialization = { status: { ready: false, phase: 'vectors', processed: 512, total: 1000 }, showOverlay: true, error: null, retrying: false, retry: vi.fn() };

describe('storage initialization overlay', () => {
  it('does not flash while a reload checks an already-ready backend', async () => {
    let finishStatusCheck;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { finishStatusCheck = resolve; }));
    function Startup() {
      const state = useStorageInitialization();
      return (
        <>
          <output>{state.ready ? 'ready' : 'waiting'}</output>
          <StorageInitializationOverlay initialization={state} />
        </>
      );
    }

    render(<Startup />);
    expect(screen.getByText('waiting')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await act(async () => {
      finishStatusCheck({ running: false, ready: true, phase: 'finalizing', processed: 0, total: 0, revision: 4, error: null });
    });
    expect(screen.getByText('ready')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('owns startup before authentication and exposes real conversion counts', () => {
    expect(pickOverlay({ auth: true, maintenance: true, storageInitialization: true })).toBe('storageInitialization');
    expect(pickOverlay({ fatal: true, storageInitialization: true })).toBe('fatal');
    render(<StorageInitializationOverlay initialization={initialization} />);
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '51');
    expect(screen.getByText('512 / 1,000 (51%)')).toBeInTheDocument();
  });

  it('keeps compaction indeterminate and errors retryable', () => {
    const { rerender } = render(<StorageInitializationOverlay initialization={{ ...initialization, status: { ready: false, phase: 'compacting', processed: 0, total: 0 } }} />);
    expect(screen.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
    expect(screen.getByText('storageStartup.phases.compacting')).toBeInTheDocument();
    rerender(<StorageInitializationOverlay initialization={{ ...initialization, error: 'disk full' }} />);
    fireEvent.click(screen.getByRole('button', { name: 'common.retry' }));
    expect(initialization.retry).toHaveBeenCalledOnce();
    rerender(<StorageInitializationOverlay initialization={{ ...initialization, status: { ready: true }, showOverlay: false }} />);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});
