import { useSyncExternalStore } from 'react';

export function setPreference(key, value) {
  if (value == null) localStorage.removeItem(key);
  else localStorage.setItem(key, String(value));
  window.dispatchEvent(new CustomEvent('settings-local-preference', { detail: { key } }));
}

export function subscribePreference(key, callback) {
  const local = (event) => { if (event.detail?.key === key) callback(); };
  const remote = (event) => { if (event.key === key || event.key === null) callback(); };
  window.addEventListener('settings-local-preference', local);
  window.addEventListener('storage', remote);
  return () => {
    window.removeEventListener('settings-local-preference', local);
    window.removeEventListener('storage', remote);
  };
}

export function usePreference(key, fallback) {
  return useSyncExternalStore(
    (callback) => subscribePreference(key, callback),
    () => localStorage.getItem(key) ?? fallback,
    () => fallback,
  );
}
