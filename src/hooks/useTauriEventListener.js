import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';

export function useTauriEventListener(eventName, handler, deps = [], enabled = true) {
  const handlerRef = useRef(handler);

  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    if (!enabled || !eventName) return undefined;

    let active = true;
    let unlistenFn = null;

    (async () => {
      try {
        const resolvedUnlisten = await listen(eventName, (event) => {
          if (active) {
            handlerRef.current?.(event);
          }
        });

        if (!active) {
          resolvedUnlisten();
          return;
        }

        unlistenFn = resolvedUnlisten;
      } catch (error) {
        if (active) {
          console.warn(`Failed to register ${eventName} listener`, error);
        }
      }
    })();

    return () => {
      active = false;
      if (unlistenFn) {
        unlistenFn();
      }
    };
  }, [eventName, enabled, ...deps]);
}
