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
});
