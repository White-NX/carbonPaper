/**
 * Stacking bands for dialogs and full-surface overlays.
 *
 * - dialog/confirmation: ordinary dialogs, hidden while a startup slot is active.
 * - prompt: questions and offers the user can answer or put off.
 * - gate: the Windows Hello lock; nothing underneath is usable until it clears.
 * - lockedAction: the settings window's explicit discard-and-close decision.
 * - maintenance: blocking index/database work. Sits above the gate on purpose,
 *   because a run that waits for authentication offers its own unlock button.
 * - fatal: the critical error window. Nothing may cover it.
 *
 * Plain class strings so Tailwind's scanner sees them.
 */
export const OVERLAY_LAYERS = {
  dialog: 'z-50',
  confirmation: 'z-[55]',
  prompt: 'z-50',
  gate: 'z-[60]',
  lockedAction: 'z-[65]',
  maintenance: 'z-[70]',
  fatal: 'z-[100]',
};

// Match the visual bands when surfaces mount or unmount in a different order.
export const OVERLAY_FOCUS_ORDER = ['dialog', 'prompt', 'confirmation', 'gate', 'lockedAction', 'maintenance', 'fatal'];
