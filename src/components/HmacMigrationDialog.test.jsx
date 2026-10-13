import React, { useEffect, useState } from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import HmacMigrationDialog from './HmacMigrationDialog';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

// App only mounts the upgrade while the session is authenticated.
function Session() {
  const [authenticated, setAuthenticated] = useState(true);
  useEffect(() => {
    const lock = () => setAuthenticated(false);
    window.addEventListener('cp-auth-required', lock);
    return () => window.removeEventListener('cp-auth-required', lock);
  }, []);
  return authenticated ? <HmacMigrationDialog /> : <button onClick={() => setAuthenticated(true)}>Unlock</button>;
}

describe('automatic search-index security upgrade', () => {
  it('waits for unlock after lost authentication and resumes on the next authenticated mount', async () => {
    let attempts = 0;
    invoke.mockReset().mockImplementation(async (command) => {
      if (command === 'storage_check_hmac_migration_status') return { needs_migration: true, is_running: false };
      if (command === 'storage_run_hmac_migration') {
        if (++attempts === 1) throw new Error('AUTH_REQUIRED');
        return true;
      }
      throw new Error(`Unexpected command: ${command}`);
    });
    render(<Session />);
    await screen.findByRole('button', { name: 'Unlock' });
    expect(attempts).toBe(1);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(invoke.mock.calls.some(([command]) => command.startsWith('credential_'))).toBe(false);

    await userEvent.click(screen.getByRole('button', { name: 'Unlock' }));
    await waitFor(() => expect(attempts).toBe(2));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      'storage_check_hmac_migration_status', 'storage_run_hmac_migration',
      'storage_check_hmac_migration_status', 'storage_run_hmac_migration',
    ]);
  });
});
