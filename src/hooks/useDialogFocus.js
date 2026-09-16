import { createContext, useContext, useLayoutEffect, useRef } from 'react';

export const DialogVisibilityContext = createContext(true);
export function useDialogVisibility(isOpen, allowWhenLocked = false) {
  return (useContext(DialogVisibilityContext) || allowWhenLocked) && isOpen;
}

const dialogs = [];
let bodyOverflow = '';
const focusable = 'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]';

// Each window has its own stack. Only the top dialog consumes Escape/Tab;
// portals keep dialogs outside settings panels and their CSS containment.
export function useDialogFocus(isOpen, onClose, disableClose = false) {
  const ref = useRef(null);
  const current = useRef({ onClose, disableClose });
  current.current = { onClose, disableClose };
  useLayoutEffect(() => {
    if (!isOpen) return undefined;
    const node = ref.current;
    const trigger = document.activeElement;
    if (!dialogs.length) bodyOverflow = document.body.style.overflow;
    dialogs.push(node);
    document.body.style.overflow = 'hidden';
    (node?.querySelector(focusable) || node)?.focus();
    const handleKey = (event) => {
      if (dialogs.at(-1) !== node) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (!current.current.disableClose) current.current.onClose?.();
      }
      if (event.key === 'Tab') {
        const elements = [...(node?.querySelectorAll(focusable) || [])].filter((element) => !element.closest('[hidden]'));
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
      const index = dialogs.indexOf(node);
      if (index >= 0) dialogs.splice(index, 1);
      document.removeEventListener('keydown', handleKey, true);
      if (!dialogs.length) document.body.style.overflow = bodyOverflow;
      if (trigger?.isConnected) trigger.focus();
    };
  }, [isOpen]);
  return ref;
}
