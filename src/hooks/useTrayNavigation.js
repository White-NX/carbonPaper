import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { takeTrayNavigation } from '../lib/tray_api';

// Subscribe before draining: a stored request survives loading/recreating the
// WebView, and notifications received during IPC trigger a subsequent drain.
export function useTrayNavigation({ enabled, onNavigate }) {
  const navigate = useRef(onNavigate);
  navigate.current = onNavigate;

  useEffect(() => {
    if (!enabled) return undefined;
    let disposed = false;
    let running = false;
    let again = false;
    let unlisten;

    const drain = async () => {
      if (disposed) return;
      if (running) { again = true; return; }
      running = true;
      try {
        do {
          again = false;
          const intent = await takeTrayNavigation();
          if (disposed) return;
          if (intent) navigate.current(intent);
        } while (again && !disposed);
      } catch (error) {
        if (!disposed && !String(error).includes('AUTH_REQUIRED')) {
          console.warn('Failed to receive tray navigation:', error);
        }
      } finally {
        running = false;
      }
    };
    void (async () => {
      try {
        const stop = await listen('tray-navigate', drain);
        if (disposed) { stop(); return; }
        unlisten = stop;
      } catch (error) {
        console.warn('Failed to listen for tray navigation:', error);
      }
      await drain();
    })();
    return () => { disposed = true; unlisten?.(); };
  }, [enabled]);
}
