import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';

/**
 * Which startup overlay wins when several want the screen, highest first.
 *
 * - fatal, maintenance and vacuum block the app outright.
 * - auth is the lock screen; nothing personal is shown before it clears.
 * - onboarding owns the required-model download on a first run, so the
 *   standalone download card ranks below it. It also outranks the update
 *   offer: the update check lands a few seconds after launch, and swapping
 *   the wizard out from under a user halfway through a step is worse than
 *   showing the update right after it.
 * - the rest are one-off questions, least intrusive last.
 */
export const OVERLAY_PRIORITY = [
  'fatal',
  'maintenance',
  'vacuum',
  'auth',
  'onboarding',
  'update',
  'modelDownload',
  'hmac',
  'clipBackfill',
  'appBound',
];

/** The id that should be on screen for a map of `{ id: wantsToShow }`. */
export function pickOverlay(requests) {
  return OVERLAY_PRIORITY.find((id) => requests[id]) ?? null;
}

const OverlayCoordinatorContext = createContext(null);
export const OverlayCoordinatorProvider = OverlayCoordinatorContext.Provider;

/**
 * State for the coordinator. The owner (App) renders
 * `<OverlayCoordinatorProvider value={coordinator.value}>` and can read
 * `coordinator.active` itself, for instance to lock the top bar.
 */
export function useOverlayCoordinator() {
  const [requests, setRequests] = useState({});
  const request = useCallback((id, wants) => {
    setRequests((previous) => (Boolean(previous[id]) === wants ? previous : { ...previous, [id]: wants }));
  }, []);
  const active = pickOverlay(requests);
  const value = useMemo(() => ({ active, request }), [active, request]);
  return { active, value };
}

/**
 * Ask for the screen and learn whether it was granted.
 *
 * An overlay reports whether it wants to be shown; it is shown only when the
 * coordinator picks it. Outside a coordinator (tests, isolated previews) every
 * request is granted.
 */
export function useOverlaySlot(id, wants) {
  const coordinator = useContext(OverlayCoordinatorContext);
  const request = coordinator?.request;
  const wanted = Boolean(wants);

  useEffect(() => {
    request?.(id, wanted);
  }, [request, id, wanted]);

  useEffect(() => () => request?.(id, false), [request, id]);

  if (!coordinator) return wanted;
  return wanted && coordinator.active === id;
}

const DEBUG_EVENT = 'debug-show-overlay';

/** Ask one overlay to render its debug preview. Development builds only. */
export function showDebugOverlay(id, variant) {
  window.dispatchEvent(new CustomEvent(DEBUG_EVENT, { detail: { id, variant } }));
}

/** Run `handler(variant)` when a debug preview of overlay `id` is requested. */
export function useDebugOverlay(id, handler) {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  useEffect(() => {
    const listener = (event) => {
      if (event.detail?.id === id) handlerRef.current(event.detail.variant);
    };
    window.addEventListener(DEBUG_EVENT, listener);
    return () => window.removeEventListener(DEBUG_EVENT, listener);
  }, [id]);
}
