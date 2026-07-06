import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import { useTauriEventListener } from './useTauriEventListener';

export function usePowerSavingState() {
  const [powerSavingMode, setPowerSavingMode] = useState(() => {
    if (typeof window === 'undefined') return true;
    const saved = localStorage.getItem('powerSavingMode');
    return saved === null ? true : saved === 'true';
  });
  const [powerSavingSuppressed, setPowerSavingSuppressed] = useState(false);
  const [windowFocused, setWindowFocused] = useState(true);

  useEffect(() => {
    localStorage.setItem('powerSavingMode', powerSavingMode ? 'true' : 'false');
  }, [powerSavingMode]);

  useTauriEventListener('power-saving-changed', (event) => {
    const payload = event.payload || {};
    setPowerSavingSuppressed(payload.active === true);
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const status = await invoke('get_power_saving_status');
        if (!cancelled) {
          setPowerSavingMode(status.enabled !== false);
          setPowerSavingSuppressed(status.active === true);
        }
      } catch (err) {
        if (!cancelled) {
          console.warn('Failed to get initial power saving status:', err);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let active = true;
    let unlistenFocus = null;
    let unlistenBlur = null;

    appWindow.listen('tauri://focus', () => {
      if (active) setWindowFocused(true);
    }).then((fn) => {
      if (active) {
        unlistenFocus = fn;
      } else {
        fn();
      }
    });
    appWindow.listen('tauri://blur', () => {
      if (active) setWindowFocused(false);
    }).then((fn) => {
      if (active) {
        unlistenBlur = fn;
      } else {
        fn();
      }
    });

    return () => {
      active = false;
      if (unlistenFocus) unlistenFocus();
      if (unlistenBlur) unlistenBlur();
    };
  }, []);

  return {
    powerSavingMode,
    setPowerSavingMode,
    powerSavingSuppressed,
    windowFocused,
  };
}

export function useWindowMaximizedState() {
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let active = true;
    let unlistenResize = null;
    const updateState = async () => {
      const maximized = await appWindow.isMaximized();
      if (active) {
        setIsMaximized(maximized);
      }
    };
    updateState();

    appWindow.listen('tauri://resize', updateState).then((fn) => {
      if (active) {
        unlistenResize = fn;
      } else {
        fn();
      }
    });

    return () => {
      active = false;
      if (unlistenResize) unlistenResize();
    };
  }, []);

  return isMaximized;
}

export function useAppWindowActions() {
  const minimize = () => getCurrentWindow().minimize();
  const toggleMaximize = () => getCurrentWindow().toggleMaximize();

  const hideToTray = async () => {
    await getCurrentWindow().hide();

    let permissionGranted = await isPermissionGranted();
    if (!permissionGranted) {
      const permission = await requestPermission();
      permissionGranted = permission === 'granted';
    }
    if (permissionGranted) {
      sendNotification({
        title: 'Carbonpaper',
        body: '程序已最小化到系统托盘，点击托盘图标可恢复窗口',
      });
    }
  };

  const restartApp = () => invoke('restart_app').catch(() => {});
  const exitApp = () => invoke('exit_app').catch(() => {});

  return {
    minimize,
    toggleMaximize,
    hideToTray,
    restartApp,
    exitApp,
  };
}
