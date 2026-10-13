import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const loadAuthApi = async () => {
  vi.resetModules();
  const core = await import('@tauri-apps/api/core');
  const mod = await import('./auth_api');
  return { ...mod, invoke: core.invoke };
};

describe('auth_api', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('caches checkAuthSession within interval', async () => {
    const { checkAuthSession, invoke } = await loadAuthApi();
    invoke.mockResolvedValue(true);

    const nowSpy = vi.spyOn(Date, 'now');
    nowSpy.mockReturnValueOnce(1000).mockReturnValueOnce(3000);

    await expect(checkAuthSession()).resolves.toBe(true);
    await expect(checkAuthSession()).resolves.toBe(true);

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('credential_check_session');

    nowSpy.mockRestore();
  });

  it('dedupes concurrent requestAuth calls', async () => {
    const { requestAuth, invoke } = await loadAuthApi();

    let resolveVerify;
    const verifyPromise = new Promise((resolve) => {
      resolveVerify = resolve;
    });

    invoke.mockImplementation((command) => {
      if (command === 'credential_verify_user') {
        return verifyPromise;
      }
      return Promise.resolve(false);
    });

    const p1 = requestAuth();
    const p2 = requestAuth();

    expect(invoke).toHaveBeenCalledTimes(1);

    resolveVerify(true);

    await expect(p1).resolves.toBe(true);
    await expect(p2).resolves.toBe(true);
  });

  it('maps user cancelled auth to AUTH_CANCELLED', async () => {
    const { requestAuth, invoke } = await loadAuthApi();
    invoke.mockRejectedValue(new Error('UserCancelled'));

    await expect(requestAuth()).rejects.toThrow('AUTH_CANCELLED');
  });

  it('emits auth-required when withAuth gets AUTH_REQUIRED and autoPrompt is off', async () => {
    const { withAuth } = await loadAuthApi();

    let fired = 0;
    const handler = () => {
      fired += 1;
    };
    window.addEventListener('cp-auth-required', handler);

    const apiCall = vi.fn(async () => {
      throw new Error('AUTH_REQUIRED');
    });

    await expect(withAuth(apiCall, { autoPrompt: false })).rejects.toThrow('AUTH_REQUIRED');
    expect(fired).toBe(1);

    window.removeEventListener('cp-auth-required', handler);
  });

  it('retries once with prompt when withAuth gets AUTH_REQUIRED', async () => {
    const { withAuth, invoke } = await loadAuthApi();

    invoke.mockImplementation((command) => {
      if (command === 'credential_verify_user') {
        return Promise.resolve(true);
      }
      return Promise.resolve(null);
    });

    const apiCall = vi
      .fn()
      .mockRejectedValueOnce(new Error('AUTH_REQUIRED'))
      .mockResolvedValueOnce('ok');

    await expect(withAuth(apiCall, { autoPrompt: true, maxRetries: 1 })).resolves.toBe('ok');
    expect(apiCall).toHaveBeenCalledTimes(2);
    expect(invoke).toHaveBeenCalledWith('credential_verify_user');
  });

  it('initializes an explicit unlock and shares its verification with a concurrent action', async () => {
    const { requestAuth, invoke } = await loadAuthApi();
    let resolveVerify;
    invoke.mockImplementation((command) => command === 'credential_verify_user'
      ? new Promise((resolve) => { resolveVerify = resolve; }) : Promise.resolve(true));
    const unlock = requestAuth({ mode: 'unlock' });
    const action = requestAuth({ mode: 'action' });
    await Promise.resolve();
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['credential_initialize', 'credential_verify_user']);
    resolveVerify(true);
    await expect(unlock).resolves.toBe(true);
    await expect(action).resolves.toBe(true);
  });

  it('retries missing-key initialization once for an explicit unlock', async () => {
    const { requestAuth, invoke } = await loadAuthApi();
    let verifications = 0;
    invoke.mockImplementation(async (command) => {
      if (command === 'credential_verify_user' && verifications++ === 0) throw new Error('Credential key not found');
      return true;
    });
    await expect(requestAuth({ mode: 'unlock' })).resolves.toBe(true);
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      'credential_initialize', 'credential_verify_user', 'credential_initialize', 'credential_verify_user',
    ]);
  });

  it('clears a cancelled prompt so the next explicit retry can succeed', async () => {
    const { requestAuth, invoke } = await loadAuthApi();
    invoke.mockRejectedValueOnce(new Error('User cancelled authentication')).mockResolvedValueOnce(true);
    await expect(requestAuth({ mode: 'gate' })).rejects.toThrow('AUTH_CANCELLED');
    await expect(requestAuth({ mode: 'gate' })).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it('maps the native unavailable message to the lock screen error contract', async () => {
    const { requestAuth, invoke } = await loadAuthApi();
    invoke.mockRejectedValueOnce(new Error('Windows Hello is not available'));
    await expect(requestAuth()).rejects.toThrow('AUTH_NOT_AVAILABLE');
  });

  it('lets explicit defer mode override the legacy automatic-prompt option', async () => {
    const { withAuth, invoke } = await loadAuthApi();
    const operation = vi.fn().mockRejectedValue(new Error('AUTH_REQUIRED'));
    await expect(withAuth(operation, { promptMode: 'defer', autoPrompt: true })).rejects.toThrow('AUTH_REQUIRED');
    expect(operation).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalled();
  });

  it('stops retrying an action after cancellation or an exhausted retry budget', async () => {
    const { withAuth, invoke } = await loadAuthApi();
    const operation = vi.fn().mockRejectedValue(new Error('AUTH_REQUIRED'));
    invoke.mockRejectedValueOnce(new Error('UserCancelled'));
    await expect(withAuth(operation, { promptMode: 'action' })).rejects.toThrow('AUTH_CANCELLED');
    expect(operation).toHaveBeenCalledTimes(1);
    invoke.mockResolvedValue(true);
    operation.mockClear();
    await expect(withAuth(operation, { promptMode: 'action' })).rejects.toThrow('AUTH_REQUIRED');
    expect(operation).toHaveBeenCalledTimes(2);
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it('does not prompt for unrelated failures or a zero retry budget', async () => {
    const { withAuth, invoke } = await loadAuthApi();
    await expect(withAuth(() => Promise.reject(new Error('DISK_FULL')), { promptMode: 'action' })).rejects.toThrow('DISK_FULL');
    await expect(withAuth(() => Promise.reject(new Error('AUTH_REQUIRED')), { promptMode: 'action', maxRetries: 0 })).rejects.toThrow('AUTH_REQUIRED');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('rejects unknown prompt policies and unbounded retry limits before running an action', async () => {
    const { withAuth, requestAuth, invoke } = await loadAuthApi();
    const operation = vi.fn();
    await expect(withAuth(operation, { promptMode: 'unknown' })).rejects.toThrow('INVALID_AUTH_PROMPT_MODE');
    await expect(withAuth(operation, { maxRetries: Infinity })).rejects.toThrow('INVALID_AUTH_RETRY_LIMIT');
    await expect(requestAuth({ mode: 'unknown' })).rejects.toThrow('INVALID_AUTH_PROMPT_MODE');
    expect(operation).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });
});
