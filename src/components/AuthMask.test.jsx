import React, { useState } from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { requestAuth } from '../lib/auth_api';
import AuthMask from './AuthMask';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../lib/auth_api', () => ({ requestAuth: vi.fn() }));

function Lock({ onSuccess }) {
  const [error, setError] = useState(null);
  return <AuthMask isVisible authError={error} setAuthError={setError} onAuthSuccess={onSuccess} />;
}

describe('lock screen authentication', () => {
  it('waits for the unlock button and retries a cancelled request through the shared API', async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    requestAuth.mockRejectedValueOnce(new Error('AUTH_CANCELLED')).mockResolvedValueOnce(true);
    render(<Lock onSuccess={onSuccess} />);
    expect(requestAuth).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'authMask.unlock_button' }));
    expect(requestAuth).toHaveBeenCalledWith({ mode: 'unlock' });
    expect(screen.getByRole('alert')).toHaveTextContent('authMask.errors.cancelled');
    expect(onSuccess).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'authMask.unlock_button' }));
    await waitFor(() => expect(onSuccess).toHaveBeenCalledTimes(1));
  });

  it('shows the unavailable-provider error without unlocking', async () => {
    const onSuccess = vi.fn();
    requestAuth.mockRejectedValueOnce(new Error('AUTH_NOT_AVAILABLE'));
    render(<Lock onSuccess={onSuccess} />);
    await userEvent.click(screen.getByRole('button', { name: 'authMask.unlock_button' }));
    expect(screen.getByRole('alert')).toHaveTextContent('authMask.errors.not_available');
    expect(onSuccess).not.toHaveBeenCalled();
  });
});
