import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

import { useAppWindowActions } from './useAppWindowState';

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(),
}));

describe('useAppWindowActions', () => {
  const hide = vi.fn();

  beforeEach(() => {
    getCurrentWindow.mockReturnValue({
      hide,
      minimize: vi.fn(),
      toggleMaximize: vi.fn(),
    });
    invoke.mockResolvedValue(undefined);
  });

  it('uses the backend tray command so hiding starts the lightweight timer', async () => {
    const { result } = renderHook(() => useAppWindowActions());

    await act(async () => {
      await result.current.hideToTray();
    });

    expect(invoke).toHaveBeenCalledWith('hide_to_tray');
    expect(hide).not.toHaveBeenCalled();
  });
});
