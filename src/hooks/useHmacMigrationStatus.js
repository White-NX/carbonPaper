import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTauriEventListener } from './useTauriEventListener';

export function useHmacMigrationStatus() {
  const [isMigrating, setIsMigrating] = useState(false);

  useEffect(() => {
    let mounted = true;
    const check = async () => {
      try {
        const status = await invoke('storage_check_hmac_migration_status');
        if (mounted && (status.needs_migration || status.is_running)) {
          setIsMigrating(true);
        }
      } catch (e) {
        console.error(e);
      }
    };
    check();

    return () => {
      mounted = false;
    };
  }, []);

  useTauriEventListener('hmac-migration-progress', () => {
    setIsMigrating(true);
  });

  useTauriEventListener('hmac-migration-complete', () => {
    setIsMigrating(false);
  });

  return isMigrating;
}
