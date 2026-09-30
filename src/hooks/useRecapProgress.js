import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getRecapProgress } from '../lib/recap_api';

export default function useRecapProgress(date, active, isAuthenticated) {
  const [state, setState] = useState({ date, progress: null, error: '' });
  useEffect(() => {
    setState({ date, progress: null, error: '' });
    if (!active || !isAuthenticated) return undefined;
    let live = true;
    let loading = false;
    let pending = false;
    const refresh = async () => {
      if (!live) return;
      if (loading) { pending = true; return; }
      loading = true;
      try {
        const progress = await getRecapProgress(date);
        if (live) setState({ date, progress, error: '' });
      } catch (error) {
        if (live) setState({ date, progress: null, error: String(error) });
      } finally {
        loading = false;
        if (pending && live) { pending = false; refresh(); }
      }
    };
    const subscription = listen('recap-progress', refresh);
    subscription.then(() => refresh()).catch((error) => {
      if (live) setState({ date, progress: null, error: String(error) });
    });
    // Recover a missed notification, including a final throttled text delta.
    const timer = setInterval(refresh, 2000);
    window.addEventListener('focus', refresh);
    return () => {
      live = false;
      clearInterval(timer);
      window.removeEventListener('focus', refresh);
      subscription.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [date, active, isAuthenticated]);
  return active && isAuthenticated && state.date === date ? state : { progress: null, error: '' };
}
