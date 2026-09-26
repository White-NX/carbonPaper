/**
 * Turn whatever a Tauri command or a thrown value produced into display text.
 *
 * Tauri rejects with plain strings, JS code throws Error objects, and a few
 * call sites reject with `{ message }` records. Every overlay used to spell
 * this out slightly differently; this is the one place that decides it.
 */
export function formatError(error) {
  if (error == null) return '';
  if (typeof error === 'string') return error;
  if (typeof error.message === 'string' && error.message) return error.message;
  return String(error);
}
