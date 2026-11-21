import React, { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, Copy } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { focusStyle } from './ui/Button';
import { MenuItem, MenuPanel } from './ui/Menu';
import {
  buildTextSelectionLines, moveTextEndpoint, textEndpointAt, textRangeQuad,
  textSelectionContent, textWordSelection,
} from '../lib/ocr_text_selection';
import './ocr-text-selection.css';

const EMPTY_RESULTS = [];
const EDITABLE_TARGETS = 'input,textarea,select,[contenteditable="true"]';
const OTHER_CONTROLS = '[data-preview-zoom-controls],[data-editor-control]';

export default function OcrTextSelection({ results = EMPTY_RESULTS, naturalSize, metrics,
  controlsRef, keyboardRef = controlsRef, onGestureStart, onSelectingChange, panReady = false }) {
  const { t } = useTranslation();
  const overlayRef = useRef(null);
  const menuRef = useRef(null);
  const menuId = useId();
  const gestureRef = useRef(null);
  const callbacksRef = useRef(null);
  callbacksRef.current = { onGestureStart, onSelectingChange, panReady };
  const copySequenceRef = useRef(0);
  const noticeTimerRef = useRef(null);
  const [state, setState] = useState(null);
  const [notice, setNotice] = useState(null);
  const [menu, setMenu] = useState(null);
  const lines = useMemo(() => buildTextSelectionLines(results, naturalSize),
    [results, naturalSize?.width, naturalSize?.height]);
  const selection = state?.lines === lines ? state : null;
  const content = useMemo(() => textSelectionContent(lines, selection), [lines, selection]);

  const closeMenu = useCallback((restoreFocus = true) => {
    setMenu(null);
    if (restoreFocus) overlayRef.current?.focus({ preventScroll: true });
  }, []);

  const isTextTarget = useCallback((target) => controlsRef?.current?.contains(target)
    && !target.closest(EDITABLE_TARGETS) && !target.closest(OTHER_CONTROLS), [controlsRef]);

  const endCapture = useCallback((cancelled = false) => {
    const gesture = gestureRef.current;
    if (!gesture) return;
    gestureRef.current = null;
    if (cancelled) setState(gesture.before);
    callbacksRef.current.onSelectingChange?.(false);
    const element = overlayRef.current;
    if (element?.hasPointerCapture?.(gesture.pointerId)) element.releasePointerCapture(gesture.pointerId);
  }, []);

  useEffect(() => {
    endCapture();
    setState(null);
    setMenu(null);
  }, [endCapture, lines]);

  useEffect(() => {
    copySequenceRef.current += 1;
    clearTimeout(noticeTimerRef.current);
    setNotice(null);
    setMenu(null);
  }, [selection]);

  useEffect(() => {
    const blur = () => { endCapture(true); closeMenu(false); };
    const outside = (event) => {
      if (menuRef.current?.contains(event.target)) return;
      closeMenu(false);
      if (event.button === 1 || event.altKey || callbacksRef.current.panReady) return;
      if (event.button === 2 && isTextTarget(event.target)) return;
      if (overlayRef.current?.contains(event.target)
        || event.target.closest('[data-preview-zoom-controls]')) return;
      setState(null);
    };
    window.addEventListener('blur', blur);
    document.addEventListener('pointerdown', outside, true);
    return () => {
      window.removeEventListener('blur', blur);
      document.removeEventListener('pointerdown', outside, true);
      endCapture();
      copySequenceRef.current += 1;
      clearTimeout(noticeTimerRef.current);
    };
  }, [closeMenu, endCapture, isTextTarget]);

  const openMenuAt = useCallback((clientX, clientY) => {
    const viewport = controlsRef?.current;
    if (!viewport || !content.text) return;
    endCapture();
    const rect = viewport.getBoundingClientRect();
    setMenu({ x: clientX - rect.left - viewport.clientLeft, y: clientY - rect.top - viewport.clientTop });
  }, [content.text, controlsRef, endCapture]);

  useEffect(() => {
    const viewport = controlsRef?.current;
    if (!viewport) return undefined;
    const contextMenu = (event) => {
      if (!content.text || !isTextTarget(event.target)) { closeMenu(false); return; }
      event.preventDefault(); event.stopPropagation();
      openMenuAt(event.clientX, event.clientY);
    };
    const wheel = (event) => {
      if (menuRef.current && !menuRef.current.contains(event.target)) closeMenu();
    };
    viewport.addEventListener('contextmenu', contextMenu);
    viewport.addEventListener('wheel', wheel, { passive: true });
    return () => {
      viewport.removeEventListener('contextmenu', contextMenu);
      viewport.removeEventListener('wheel', wheel);
    };
  }, [closeMenu, content.text, controlsRef, isTextTarget, openMenuAt]);

  useLayoutEffect(() => {
    const element = menuRef.current;
    const viewport = controlsRef?.current;
    if (!menu || !element || !viewport) return undefined;
    const fit = () => {
      const rect = viewport.getBoundingClientRect();
      const width = viewport.clientWidth || rect.width;
      const height = viewport.clientHeight || rect.height;
      element.querySelector('[role="menu"]').style.maxHeight = `${Math.max(0, height - 16)}px`;
      const bounds = element.getBoundingClientRect();
      element.style.left = `${Math.max(8, Math.min(menu.x, width - bounds.width - 8))}px`;
      element.style.top = `${Math.max(8, Math.min(menu.y, height - bounds.height - 8))}px`;
    };
    fit();
    element.querySelector('[role="menuitem"]')?.focus({ preventScroll: true });
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(fit) : null;
    observer?.observe(viewport);
    observer?.observe(element);
    window.addEventListener('resize', fit);
    return () => { observer?.disconnect(); window.removeEventListener('resize', fit); };
  }, [controlsRef, menu]);

  const copy = useCallback(async () => {
    if (!content.text) return;
    closeMenu();
    const sequence = ++copySequenceRef.current;
    try {
      await navigator.clipboard.writeText(content.text);
      if (copySequenceRef.current === sequence) setNotice('copied');
    } catch {
      if (copySequenceRef.current === sequence) setNotice('copyFailed');
    }
    if (copySequenceRef.current === sequence) {
      clearTimeout(noticeTimerRef.current);
      noticeTimerRef.current = setTimeout(() => setNotice(null), 2000);
    }
  }, [closeMenu, content.text]);

  useEffect(() => {
    const keyDown = (event) => {
      if (event.defaultPrevented || event.target.closest(EDITABLE_TARGETS) || event.isComposing
        || document.querySelector('[role="dialog"][aria-modal="true"]')
        || !(overlayRef.current?.contains(event.target) || menuRef.current?.contains(event.target)
          || keyboardRef?.current?.contains(event.target))) return;
      const key = event.key.toLowerCase();
      const textTarget = overlayRef.current?.contains(event.target) || menuRef.current?.contains(event.target)
        || isTextTarget(event.target);
      if ((event.ctrlKey || event.metaKey) && key === 'c' && content.text) {
        event.preventDefault(); event.stopPropagation(); void copy();
      } else if ((event.ctrlKey || event.metaKey) && key === 'a' && lines.length) {
        event.preventDefault(); event.stopPropagation();
        endCapture(); closeMenu();
        setState({ lines, all: true, anchor: { line: 0, offset: 0 },
          focus: { line: lines.length - 1, offset: lines.at(-1).text.length } });
      } else if ((key === 'contextmenu' || (key === 'f10' && event.shiftKey)) && content.text && textTarget
        && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault(); event.stopPropagation();
        const line = lines[selection.focus.line];
        const position = line.positions[line.offsets.indexOf(selection.focus.offset)];
        const quad = textRangeQuad(line, position, position);
        const rect = overlayRef.current.getBoundingClientRect();
        if (quad) openMenuAt(rect.left + quad[3][0] * rect.width / naturalSize.width,
          rect.top + quad[3][1] * rect.height / naturalSize.height);
      } else if (key === 'escape' && menu) {
        event.preventDefault(); event.stopPropagation(); closeMenu();
      } else if (key === 'escape' && selection && textTarget) {
        event.preventDefault(); event.stopPropagation(); endCapture(); setState(null);
      } else if (selection && textTarget && ['arrowleft', 'arrowright', 'home', 'end'].includes(key)
        && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault(); event.stopPropagation();
        const focus = key === 'home' || key === 'end'
          ? { line: selection.focus.line, offset: key === 'home' ? 0 : lines[selection.focus.line].text.length }
          : moveTextEndpoint(lines, selection.focus, key === 'arrowleft' ? -1 : 1, { all: selection.all });
        setState({ lines, all: event.shiftKey && selection.all, anchor: event.shiftKey ? selection.anchor : focus, focus });
      }
    };
    document.addEventListener('keydown', keyDown);
    return () => document.removeEventListener('keydown', keyDown);
  }, [closeMenu, content.text, copy, endCapture, isTextTarget, keyboardRef, lines, menu, naturalSize.width, naturalSize.height, openMenuAt, selection]);

  const pointAt = (event) => {
    const rect = overlayRef.current.getBoundingClientRect();
    if (!rect.width || !rect.height) return null;
    return [(event.clientX - rect.left) * naturalSize.width / rect.width,
      (event.clientY - rect.top) * naturalSize.height / rect.height];
  };

  const pointerDown = (event) => {
    if (event.button !== 0 || event.altKey || panReady || gestureRef.current || event.isPrimary === false) return;
    const point = pointAt(event);
    const lineIndex = Number(event.target.closest('[data-ocr-line]')?.getAttribute('data-ocr-line'));
    const endpoint = point && textEndpointAt(lines, point, { lineIndex });
    if (!endpoint) return;
    event.preventDefault(); event.stopPropagation();
    window.getSelection()?.removeAllRanges();
    callbacksRef.current.onGestureStart?.();
    overlayRef.current.focus({ preventScroll: true });
    const region = lines[endpoint.line].region;
    const anchor = event.shiftKey && selection && !selection.all && lines[selection.anchor.line].region === region
      ? selection.anchor : endpoint;
    gestureRef.current = { pointerId: event.pointerId, anchor, region, focus: endpoint, before: selection };
    overlayRef.current.setPointerCapture?.(event.pointerId);
    callbacksRef.current.onSelectingChange?.(true);
    setState({ lines, anchor, focus: endpoint });
  };

  const moveFocus = (event) => {
    const gesture = gestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    event.preventDefault(); event.stopPropagation();
    const point = pointAt(event);
    const focus = point && textEndpointAt(lines, point, { region: gesture.region, preferredLine: gesture.focus.line });
    if (focus) { gesture.focus = focus; setState({ lines, anchor: gesture.anchor, focus }); }
  };

  const selectWord = (event) => {
    if (event.button !== 0 || event.altKey || panReady) return;
    event.preventDefault(); event.stopPropagation();
    const point = pointAt(event);
    const lineIndex = Number(event.target.closest('[data-ocr-line]')?.getAttribute('data-ocr-line'));
    const endpoint = point && textEndpointAt(lines, point, { lineIndex, character: true });
    if (endpoint) setState({ lines, ...textWordSelection(lines, endpoint) });
  };

  if (!lines.length) return null;
  const bounds = metrics ? { left: metrics.offsetX, top: metrics.offsetY,
    width: metrics.renderWidth, height: metrics.renderHeight } : { inset: 0 };
  const caret = selection && !content.text ? selection.focus : null;
  const caretLine = caret && lines[caret.line];
  const caretPosition = caretLine?.positions[caretLine.offsets.indexOf(caret.offset)];
  const caretQuad = caretLine && textRangeQuad(caretLine, caretPosition, caretPosition);
  const controls = controlsRef?.current ? createPortal(<>
    {menu && content.text && <div ref={menuRef} data-ocr-selection-menu data-ocr-selection-controls data-editor-control
      className="ocr-text-menu absolute z-30" style={{ left: menu.x, top: menu.y, maxWidth: 'calc(100% - 16px)', maxHeight: 'calc(100% - 16px)' }}
      onPointerDown={(event) => event.stopPropagation()} onDoubleClick={(event) => event.stopPropagation()}
      onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) closeMenu(false); }}
      onKeyDown={(event) => {
        if (event.key === 'Escape') {
          event.preventDefault(); event.stopPropagation(); closeMenu();
        } else if (['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
          event.preventDefault(); event.stopPropagation(); menuRef.current?.querySelector('[role="menuitem"]')?.focus();
        } else if (event.key === ' ' || event.key === 'Enter') event.stopPropagation();
      }}>
      <MenuPanel id={menuId} role="menu" aria-label={t('snapshotPreview.textActions')}
        className="relative right-auto top-auto m-0 w-48 min-w-0 max-w-full overflow-y-auto overscroll-contain rounded-xl p-1.5 preview-action-bar">
        <MenuItem role="menuitem" aria-label={t('snapshotPreview.copy')} aria-keyshortcuts="Control+C"
          icon={<Copy className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />}
          className={`rounded-lg py-2 text-ide-text focus-visible:bg-ide-hover ${focusStyle}`}
          onClick={copy} title={t('snapshotPreview.copySelectedShortcut')}>
          <span className="min-w-0 flex-1 truncate">{t('snapshotPreview.copy')}</span>
          {content.ranges.some((range) => range.estimated) && <span className="ocr-text-estimate" role="img"
            aria-label={t('snapshotPreview.estimatedSelection')} title={t('snapshotPreview.estimatedSelection')}>≈</span>}
          <span className="shrink-0 text-[11px] text-ide-muted" aria-hidden="true">Ctrl+C</span>
        </MenuItem>
      </MenuPanel>
    </div>}
    {notice && <div role={notice === 'copyFailed' ? 'alert' : 'status'}
      className={`ocr-text-notice pointer-events-none absolute bottom-4 left-1/2 z-20 flex max-w-[calc(100%-32px)] -translate-x-1/2 items-center gap-1.5 rounded-full border border-ide-border px-3 py-1.5 text-xs preview-action-bar ${notice === 'copyFailed' ? 'text-ide-error' : 'text-ide-text'}`}>
      {notice === 'copied' && <Check className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />}
      {t(`snapshotPreview.${notice}`)}
    </div>}
  </>, controlsRef.current) : null;

  return <>
    <div ref={overlayRef} data-ocr-selection className="ocr-text-selection absolute" style={bounds}
      tabIndex={0} role="group" aria-label={t('snapshotPreview.selectText')}
      aria-haspopup="menu" aria-expanded={Boolean(menu && content.text)} aria-controls={menu && content.text ? menuId : undefined}
      onPointerDown={pointerDown} onPointerMove={moveFocus}
      onPointerUp={(event) => { moveFocus(event); if (gestureRef.current?.pointerId === event.pointerId) endCapture(); }}
      onPointerCancel={(event) => { if (gestureRef.current?.pointerId === event.pointerId) endCapture(true); }}
      onLostPointerCapture={(event) => { if (gestureRef.current?.pointerId === event.pointerId) endCapture(true); }}
      onDoubleClick={selectWord}
      onClick={(event) => {
        if (event.detail < 3 || event.altKey || panReady) return;
        event.stopPropagation();
        const index = Number(event.target.closest('[data-ocr-line]')?.getAttribute('data-ocr-line'));
        if (lines[index]) setState({ lines, anchor: { line: index, offset: 0 }, focus: { line: index, offset: lines[index].text.length } });
      }}>
      <svg width="100%" height="100%" viewBox={`0 0 ${naturalSize.width} ${naturalSize.height}`} preserveAspectRatio="none" aria-hidden="true">
        {lines.map((line, index) => <polygon key={line.id} data-ocr-line={index} className="ocr-text-hit"
          style={panReady ? { pointerEvents: 'none' } : undefined}
          points={line.quad.map((point) => point.join(',')).join(' ')}><title>{line.text}</title></polygon>)}
        {content.ranges.map((range) => <polygon key={range.line} data-ocr-highlight
          className="ocr-text-highlight" points={range.quad.map((point) => point.join(',')).join(' ')} />)}
        {caretQuad && <line className="ocr-text-caret" x1={caretQuad[0][0]} y1={caretQuad[0][1]}
          x2={caretQuad[3][0]} y2={caretQuad[3][1]} vectorEffect="non-scaling-stroke" />}
      </svg>
    </div>
    {controls}
  </>;
}
