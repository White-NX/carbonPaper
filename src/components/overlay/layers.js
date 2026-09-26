/**
 * Stacking order for full-surface overlays, lowest first.
 *
 * - prompt: questions and offers the user can answer or put off.
 * - gate: the Windows Hello lock; nothing underneath is usable until it clears.
 * - maintenance: blocking index/database work. Sits above the gate on purpose,
 *   because a run that waits for authentication offers its own unlock button.
 * - fatal: the critical error window. Nothing may cover it.
 *
 * Plain class strings so Tailwind's scanner sees them.
 */
export const OVERLAY_LAYERS = {
  prompt: 'z-50',
  gate: 'z-[60]',
  maintenance: 'z-[70]',
  fatal: 'z-[100]',
};
