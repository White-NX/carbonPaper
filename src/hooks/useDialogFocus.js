import { createContext, useContext, useLayoutEffect, useRef } from 'react';
import { useActiveOverlay } from '../components/overlay/coordinator';
import { OVERLAY_FOCUS_ORDER } from '../components/overlay/layers';

export const DialogVisibilityContext = createContext(true);
export function useDialogVisibility(isOpen, allowWhenLocked = false) {
  const authenticated = useContext(DialogVisibilityContext);
  const activeOverlay = useActiveOverlay();
  const hasScreen = activeOverlay === null || (allowWhenLocked && activeOverlay === 'auth');
  return Boolean(isOpen && (authenticated || allowWhenLocked) && hasScreen);
}

const dialogs = [];
let bodyOverflow = '';
const isolated = new Map();
const focusable = 'button, input:not([type="hidden"]), select, textarea, a[href], [tabindex], [contenteditable="true"]';

function canFocus(element) {
  if (!element?.isConnected || element.matches(':disabled') || element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
  for (let ancestor = element; ancestor; ancestor = ancestor.parentElement) {
    const style = window.getComputedStyle(ancestor);
    if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') return false;
  }
  return true;
}

function focusableElements(node) {
  return [...node.querySelectorAll(focusable)].filter((element) => element.tabIndex >= 0 && canFocus(element));
}

function topDialog() {
  return dialogs.reduce((top, entry) => (!top || entry.priority >= top.priority ? entry : top), null);
}

function restoreIsolation() {
  for (const [element, attributes] of isolated) {
    for (const [name, value] of Object.entries(attributes)) {
      if (value === null) element.removeAttribute(name);
      else element.setAttribute(name, value);
    }
  }
  isolated.clear();
}

function isolateBackground(entry) {
  if (!entry) return;
  const boundary = entry.scope === 'window' ? document.body : entry.node.parentElement?.parentElement;
  // A body portal isolates the window. A startup card only isolates its
  // positioned content area, leaving the title bar and window controls usable.
  for (let branch = entry.node; branch?.parentElement && branch !== boundary; branch = branch.parentElement) {
    for (const sibling of branch.parentElement.children) {
      if (sibling === branch) continue;
      isolated.set(sibling, { inert: sibling.getAttribute('inert'), 'aria-hidden': sibling.getAttribute('aria-hidden') });
      sibling.setAttribute('inert', '');
      sibling.setAttribute('aria-hidden', 'true');
    }
  }
}

function focusDialog(entry, preferred) {
  const { node } = entry;
  if (preferred && node.contains(preferred) && canFocus(preferred)) preferred.focus();
  else if (!node.contains(document.activeElement) || !canFocus(document.activeElement)) {
    (focusableElements(node)[0] || node).focus();
  }
}

// Each window has its own stack. Only the top dialog consumes Escape/Tab;
// portals keep dialogs outside settings panels and their CSS containment.
export function useDialogFocus(isOpen, onClose, disableClose = false, { layer = 'dialog', scope = 'container' } = {}) {
  const ref = useRef(null);
  const current = useRef({ onClose, disableClose });
  current.current = { onClose, disableClose };
  useLayoutEffect(() => {
    if (!isOpen) return undefined;
    const node = ref.current;
    if (!node) return undefined;
    const trigger = document.activeElement;
    if (!dialogs.length) bodyOverflow = document.body.style.overflow;
    const entry = { node, scope, priority: OVERLAY_FOCUS_ORDER.indexOf(layer) };
    dialogs.push(entry);
    document.body.style.overflow = 'hidden';
    restoreIsolation();
    const top = topDialog();
    focusDialog(top);
    isolateBackground(top);
    const handleKey = (event) => {
      if (topDialog() !== entry) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (!current.current.disableClose) current.current.onClose?.();
      }
      if (event.key === 'Tab') {
        const elements = focusableElements(node);
        if (!elements.length) { event.preventDefault(); node?.focus(); return; }
        const first = elements[0];
        const last = elements.at(-1);
        if (event.shiftKey && (document.activeElement === first || !node.contains(document.activeElement))) {
          event.preventDefault(); last.focus();
        } else if (!event.shiftKey && (document.activeElement === last || !node.contains(document.activeElement))) {
          event.preventDefault(); first.focus();
        }
      }
    };
    document.addEventListener('keydown', handleKey, true);
    return () => {
      const wasTop = topDialog() === entry;
      const index = dialogs.indexOf(entry);
      if (index >= 0) dialogs.splice(index, 1);
      document.removeEventListener('keydown', handleKey, true);
      if (!dialogs.length) document.body.style.overflow = bodyOverflow;
      restoreIsolation();
      const next = topDialog();
      if (wasTop) {
        if (next) focusDialog(next, trigger);
        else if (canFocus(trigger)) trigger.focus();
      }
      isolateBackground(next);
    };
  }, [isOpen, layer, scope]);
  return ref;
}
