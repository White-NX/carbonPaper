import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, Copy, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from './ui/Button';
import {
  buildTextSelectionLines, moveTextEndpoint, textEndpointAt, textRangeQuad,
  textSelectionContent, textWordSelection,
} from '../lib/ocr_text_selection';
import './ocr-text-selection.css';

const EMPTY_RESULTS = [];
const EDITABLE_TARGETS = 'input,textarea,select,[contenteditable="true"]';

export default function OcrTextSelection({ results = EMPTY_RESULTS, naturalSize, metrics,
  controlsRef, onGestureStart, onSelectingChange, panReady = false }) {
  const { t } = useTranslation();
  const overlayRef = useRef(null);
  const toolbarRef = useRef(null);
  const gestureRef = useRef(null);
  const callbacksRef = useRef(null);
  callbacksRef.current = { onGestureStart, onSelectingChange, panReady };
  const copySequenceRef = useRef(0);
  const noticeTimerRef = useRef(null);
  const [state, setState] = useState(null);
  const [notice, setNotice] = useState(null);
  const lines = useMemo(() => buildTextSelectionLines(results, naturalSize),
    [results, naturalSize?.width, naturalSize?.height]);
  const selection = state?.lines === lines ? state : null;
  const content = useMemo(() => textSelectionContent(lines, selection), [lines, selection]);

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
  }, [endCapture, lines]);

  useEffect(() => {
    copySequenceRef.current += 1;
    clearTimeout(noticeTimerRef.current);
    setNotice(null);
  }, [selection]);

  useEffect(() => {
    const blur = () => endCapture(true);
    const outside = (event) => {
      if (event.button === 1 || event.altKey || callbacksRef.current.panReady) return;
      if (overlayRef.current?.contains(event.target) || toolbarRef.current?.contains(event.target)
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
  }, [endCapture]);

  const copy = useCallback(async () => {
    if (!content.text) return;
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
  }, [content.text]);

  useEffect(() => {
    const keyDown = (event) => {
      if (event.target.closest(EDITABLE_TARGETS) || event.isComposing
        || document.querySelector('[role="dialog"][aria-modal="true"]')
        || !(overlayRef.current?.contains(event.target) || toolbarRef.current?.contains(event.target)
          || controlsRef?.current?.contains(event.target))) return;
      const key = event.key.toLowerCase();
      if ((event.ctrlKey || event.metaKey) && key === 'c' && content.text) {
        event.preventDefault(); event.stopPropagation(); void copy();
      } else if ((event.ctrlKey || event.metaKey) && key === 'a' && lines.length) {
        event.preventDefault(); event.stopPropagation();
        setState({ lines, anchor: { line: 0, offset: 0 },
          focus: { line: lines.length - 1, offset: lines.at(-1).text.length } });
      } else if (key === 'escape' && selection) {
        event.preventDefault(); event.stopPropagation(); endCapture(); setState(null);
      } else if (selection && ['arrowleft', 'arrowright', 'home', 'end'].includes(key)
        && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault(); event.stopPropagation();
        const focus = key === 'home' || key === 'end'
          ? { line: selection.focus.line, offset: key === 'home' ? 0 : lines[selection.focus.line].text.length }
          : moveTextEndpoint(lines, selection.focus, key === 'arrowleft' ? -1 : 1);
        setState({ lines, anchor: event.shiftKey ? selection.anchor : focus, focus });
      }
    };
    document.addEventListener('keydown', keyDown);
    return () => document.removeEventListener('keydown', keyDown);
  }, [content.text, controlsRef, copy, endCapture, lines, selection]);

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
    const anchor = event.shiftKey && selection ? selection.anchor : endpoint;
    gestureRef.current = { pointerId: event.pointerId, anchor, before: selection };
    overlayRef.current.setPointerCapture?.(event.pointerId);
    callbacksRef.current.onSelectingChange?.(true);
    setState({ lines, anchor, focus: endpoint });
  };

  const moveFocus = (event) => {
    const gesture = gestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    event.preventDefault(); event.stopPropagation();
    const point = pointAt(event);
    const focus = point && textEndpointAt(lines, point, { preferredLine: gesture.anchor.line });
    if (focus) setState({ lines, anchor: gesture.anchor, focus });
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
  const toolbar = content.text && controlsRef?.current ? createPortal(
    <div ref={toolbarRef} data-ocr-selection-controls data-editor-control
      className="ocr-text-toolbar absolute left-1/2 top-3 z-20 flex -translate-x-1/2 items-center gap-0.5 rounded-full border border-ide-border bg-ide-panel/95 px-1.5 py-1 shadow-lg"
      onPointerDown={(event) => event.stopPropagation()} onDoubleClick={(event) => event.stopPropagation()}>
      <Button icon={notice === 'copied' ? Check : Copy} variant="ghost" size="xs" className="rounded-full border-0 text-ide-text"
        onClick={copy} title={t('snapshotPreview.copySelectedShortcut')}>
        {t(notice ? `snapshotPreview.${notice}` : 'snapshotPreview.copySelectedText')}
      </Button>
      {content.ranges.some((range) => range.estimated) && <span className="ocr-text-estimate" role="img" aria-label={t('snapshotPreview.estimatedSelection')}
        title={t('snapshotPreview.estimatedSelection')}>≈</span>}
      <Button icon={X} variant="ghost" size="xs" className="rounded-full border-0"
        onClick={() => { setState(null); overlayRef.current?.focus({ preventScroll: true }); }}
        title={t('snapshotPreview.clearTextSelection')} aria-label={t('snapshotPreview.clearTextSelection')} />
    </div>, controlsRef.current) : null;

  return <>
    <div ref={overlayRef} data-ocr-selection className="ocr-text-selection absolute" style={bounds}
      tabIndex={0} role="group" aria-label={t('snapshotPreview.selectText')}
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
    {toolbar}
  </>;
}
