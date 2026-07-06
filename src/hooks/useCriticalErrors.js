import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTauriEventListener } from './useTauriEventListener';

export function useCriticalErrors() {
  const [criticalErrors, setCriticalErrors] = useState([]);
  const [criticalErrorLogPath, setCriticalErrorLogPath] = useState('');

  useTauriEventListener('critical-error', (event) => {
    const msg = event.payload?.message || event.payload || 'Unknown error';
    setCriticalErrors((prev) => [...prev, msg]);
    invoke('get_log_dir').then(setCriticalErrorLogPath).catch(() => { });
  });

  return { criticalErrors, criticalErrorLogPath };
}
