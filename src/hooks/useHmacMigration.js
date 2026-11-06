import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { usePolling } from './usePolling';
import { useTauriEventListener } from './useTauriEventListener';

export function useHmacMigration() {
  const [status, setStatus] = useState(null);
  const [dismissed, setDismissed] = useState(false);
  const latestRevision = useRef(-1);
  const alive = useRef(false);

  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  const accept = useCallback((next) => {
    if (!alive.current || !next) return;
    if (Number.isFinite(next.revision)) {
      if (next.revision < latestRevision.current) return;
      latestRevision.current = next.revision;
    }
    setStatus((previous) => ({ ...previous, ...next }));
  }, []);

  const refresh = useCallback(async () => {
    try {
      const next = await invoke('storage_check_hmac_migration_status');
      accept(next);
      return next?.needs_migration || next?.is_running;
    } catch {
      return true;
    }
  }, [accept]);

  usePolling(refresh, {
    intervalMs: 1000,
    enabled: !status?.error && (status?.needs_migration !== false || status?.is_running),
  });
  useTauriEventListener('hmac-migration-progress', ({ payload }) => accept(payload), [accept]);
  useTauriEventListener('hmac-migration-complete', refresh, [refresh]);

  return {
    open: !dismissed && Boolean(status?.is_running || status?.error),
    progress: status || { processed: 0, total: 0 },
    error: status?.error,
    dismiss: () => setDismissed(true),
  };
}
