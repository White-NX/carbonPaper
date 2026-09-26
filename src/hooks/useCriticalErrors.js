import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

function toCriticalError(payload) {
  return { id: payload?.id, message: payload?.message || 'Unknown error' };
}

// The backend numbers every error, so one that arrives both in the record and
// as an event is shown once, in the order it was reported.
function mergeCriticalErrors(current, incoming) {
  const known = new Set(current.map((error) => error.id));
  const added = incoming.filter((error) => !known.has(error.id));
  if (added.length === 0) return current;
  return [...current, ...added].sort((a, b) => a.id - b.id);
}

export function useCriticalErrors() {
  const [errors, setErrors] = useState([]);
  const [criticalErrorLogPath, setCriticalErrorLogPath] = useState('');
  const hasErrors = errors.length > 0;

  useEffect(() => {
    let active = true;
    let unlisten = null;
    const add = (incoming) => {
      if (active) setErrors((current) => mergeCriticalErrors(current, incoming));
    };

    (async () => {
      try {
        const resolvedUnlisten = await listen('critical-error', (event) => {
          add([toCriticalError(event.payload)]);
        });
        if (!active) {
          resolvedUnlisten();
          return;
        }
        unlisten = resolvedUnlisten;
      } catch (error) {
        console.warn('Failed to register critical-error listener', error);
      }
      // A main window created after an error was reported (lightweight mode
      // destroys it) learns about that error only from the backend's record.
      // Read it once the listener is in place, so an error reported in between
      // arrives by one route or the other.
      try {
        const retained = await invoke('get_critical_errors');
        add((retained || []).map(toCriticalError));
      } catch {
        // Errors that arrive as events are still shown.
      }
    })();

    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    if (!hasErrors) return;
    invoke('get_log_dir').then(setCriticalErrorLogPath).catch(() => { });
  }, [hasErrors]);

  return { criticalErrors: errors.map((error) => error.message), criticalErrorLogPath };
}
