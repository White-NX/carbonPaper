import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { CircleCheck, Loader2, Pencil, RotateCw, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useSnapshotEditor } from '../hooks/useSnapshotEditor';
import { retryEditedScreenshotOcr } from '../lib/monitor_api';
import {
  clamp, cloneScene, constrainObject, eraseScene, hitObject, objectBounds, renderScene,
  resizeObject, TEXT_FONT, TEXT_LEADING, TEXT_PADDING, textErasureMask,
} from '../lib/snapshot_editor';
import { InspectorOverlay } from './InspectorImage';
import PreviewActionBar from './PreviewActionBar';
import PreviewZoomControls from './PreviewZoomControls';
import ConfirmDialog from './ConfirmDialog';
import { Button } from './ui/Button';
import './snapshot-editor.css';

let nextObjectId = 0;
const PAN_THRESHOLD = 4;
const EDITABLE_TARGETS = 'input,textarea,select,[contenteditable="true"]';
const CORNER_LABELS = { nw: 'snapshotEditor.corners.nw', ne: 'snapshotEditor.corners.ne',
  se: 'snapshotEditor.corners.se', sw: 'snapshotEditor.corners.sw' };

export default function SnapshotEditor({ imageSrc, selectedEvent, selectedDetails, overlayBoxes,
  onBoxClick, actionBarProps, isAuthenticated, onSaved, onNavigationGuardChange }) {
  const { t } = useTranslation();
  const rootRef = useRef(null);
  const viewportRef = useRef(null);
  const imageRef = useRef(null);
  const canvasRef = useRef(null);
  const renderScaleRef = useRef(1);
  const textRef = useRef(null);
  const gestureRef = useRef(null);
  const spacePressedRef = useRef(false);
  const loadedImageIdRef = useRef(null);
  const [image, setImage] = useState(null);
  const [imageError, setImageError] = useState(false);
  const [viewport, setViewport] = useState({ width: 0, height: 0 });
  const [view, setView] = useState({ scale: 1, x: 0, y: 0 });
  const [animateView, setAnimateView] = useState(false);
  const [panning, setPanning] = useState(false);
  const [spacePressed, setSpacePressed] = useState(false);
  const [hoveringObject, setHoveringObject] = useState(false);
  const [pointerPosition, setPointerPosition] = useState(null);
  const [interacting, setInteracting] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const [retryError, setRetryError] = useState(false);
  const editor = useSnapshotEditor({ image, screenshotId: selectedEvent.id,
    imageHash: selectedDetails?.record?.image_hash,
    enabled: isAuthenticated && selectedDetails?.record?.status === 'committed',
    onSaved, onNavigationGuardChange, placeholder: t('snapshotEditor.placeholder') });
  const width = image?.naturalWidth || 1;
  const height = image?.naturalHeight || 1;
  const fitScale = Math.min(viewport.width / width, viewport.height / height, 1) || 1;
  const actualScale = fitScale * view.scale;
  renderScaleRef.current = actualScale;

  useEffect(() => {
    let disposed = false;
    const original = new Image();
    original.onload = () => {
      if (disposed) return;
      setImage(original);
      setImageError(false);
      setAnimateView(false);
      if (loadedImageIdRef.current !== selectedEvent.id) setView({ scale: 1, x: 0, y: 0 });
      loadedImageIdRef.current = selectedEvent.id;
    };
    original.onerror = () => { if (!disposed) setImageError(true); };
    original.src = imageSrc;
    return () => { disposed = true; original.onload = null; original.onerror = null; };
  }, [imageSrc, selectedEvent.id]);

  useLayoutEffect(() => {
    const element = viewportRef.current;
    const resize = () => setViewport({ width: element.clientWidth, height: element.clientHeight });
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    if (!image || !canvasRef.current) return;
    renderScene(canvasRef.current, editor.baseImage || image, editor.scene, { editingId: editor.editingId });
    const gesture = gestureRef.current;
    if (gesture?.type === 'create') {
      const node = gesture.node;
      const context = canvasRef.current.getContext('2d');
      context.strokeStyle = getComputedStyle(rootRef.current).getPropertyValue('--ide-accent').trim();
      context.lineWidth = 1 / renderScaleRef.current;
      context.setLineDash([4 / renderScaleRef.current, 3 / renderScaleRef.current]);
      context.strokeRect(node.cx - node.w / 2, node.cy - node.h / 2, node.w, node.h);
      context.setLineDash([]);
    }
  }, [image, editor.baseImage, editor.revision, editor.scene, editor.editingId]);

  useLayoutEffect(() => {
    if (editor.editingId) {
      textRef.current?.focus({ preventScroll: true });
      const input = textRef.current;
      if (input) input.setSelectionRange(input.value.length, input.value.length);
    }
  }, [editor.editingId]);

  const pointAt = useCallback((event) => {
    const rect = imageRef.current.getBoundingClientRect();
    return [(event.clientX - rect.left) * width / rect.width,
      (event.clientY - rect.top) * height / rect.height];
  }, [height, width]);

  // A pointer gesture takes over the displayed view, including a wheel zoom
  // still in flight, so annotations and dragging never jump to its target.
  const freezeView = () => {
    const rect = imageRef.current.getBoundingClientRect();
    const viewportRect = viewportRef.current.getBoundingClientRect();
    if (!rect.width) return view;
    const displayed = { scale: rect.width / width / fitScale,
      x: rect.left - viewportRect.left + rect.width / 2 - viewport.width / 2,
      y: rect.top - viewportRect.top + rect.height / 2 - viewport.height / 2 };
    setView(displayed);
    return displayed;
  };

  const changeZoom = useCallback((requested, anchor = null) => {
    setAnimateView(true);
    setView((previous) => {
      const next = clamp(typeof requested === 'function' ? requested(previous.scale) : requested, 0.2, 10);
      const ratio = next / previous.scale;
      const x = anchor ? anchor[0] - viewport.width / 2 : 0;
      const y = anchor ? anchor[1] - viewport.height / 2 : 0;
      return { scale: next,
        x: clamp(x - (x - previous.x) * ratio, -width * fitScale * next, width * fitScale * next),
        y: clamp(y - (y - previous.y) * ratio, -height * fitScale * next, height * fitScale * next) };
    });
  }, [fitScale, height, viewport, width]);

  const resetZoom = () => {
    setAnimateView(true);
    setView({ scale: 1, x: 0, y: 0 });
  };

  useEffect(() => {
    const element = viewportRef.current;
    const wheel = (event) => {
      if (editor.stateRef.current.busy || gestureRef.current) return;
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      const delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? element.clientHeight : 1);
      changeZoom((scale) => scale * Math.exp(-delta * 0.0015), [event.clientX - rect.left, event.clientY - rect.top]);
    };
    element.addEventListener('wheel', wheel, { passive: false });
    return () => element.removeEventListener('wheel', wheel);
  }, [changeZoom, editor.stateRef]);

  useEffect(() => {
    if (!editor.editing || !isAuthenticated) return undefined;
    const handleKey = (event) => {
      if (editor.stateRef.current.busy || document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      if (event.target.closest(EDITABLE_TARGETS)) {
        if (event.key === 'Escape' && event.target === textRef.current) {
          event.preventDefault();
          editor.commitText();
          viewportRef.current.focus({ preventScroll: true });
        }
        return;
      }
      if (event.ctrlKey || event.metaKey) {
        const key = event.key.toLowerCase();
        if (key === 'z') { event.preventDefault(); event.shiftKey ? editor.redoAction() : editor.undoAction(); }
        else if (key === 'y') { event.preventDefault(); editor.redoAction(); }
        else if (key === 's') { event.preventDefault(); void editor.save(); }
      } else if (event.key === 'Delete' || event.key === 'Backspace') {
        event.preventDefault(); editor.removeSelected();
      } else if (event.key === 'Escape') {
        event.preventDefault();
        if (editor.stateRef.current.selectedId) editor.select(null);
        else editor.requestLeave();
      }
    };
    document.addEventListener('keydown', handleKey);
    return () => document.removeEventListener('keydown', handleKey);
  }, [editor, isAuthenticated]);

  const capturePointer = (event, gesture) => {
    gestureRef.current = { ...gesture, pointerId: event.pointerId };
    setInteracting(true);
    viewportRef.current.setPointerCapture(event.pointerId);
  };

  const beginPan = (event, displayed) => {
    event.stopPropagation();
    setHoveringObject(false);
    capturePointer(event, { type: 'pan', start: [event.clientX, event.clientY], original: displayed, moved: false });
  };

  const beginTransform = (event, type, corner = null) => {
    if (editor.busy || gestureRef.current || event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    editor.commitText();
    const current = editor.stateRef.current;
    const node = current.scene.find((item) => item.id === current.selectedId);
    if (!node) return;
    const start = pointAt(event);
    freezeView();
    capturePointer(event, { type, nodeId: node.id, before: cloneScene(current.scene), original: structuredClone(node), start, corner,
      startAngle: Math.atan2(start[1] - node.cy, start[0] - node.cx) });
  };

  const pointerDown = (event) => {
    if (!image || editor.busy || gestureRef.current || ![0, 1].includes(event.button)) return;
    if (event.target.closest(EDITABLE_TARGETS)) return;
    const forcePan = event.button === 1 || event.altKey || spacePressedRef.current;
    if (event.target.closest('[data-editor-control]')
      && !(forcePan && event.target.closest('.snapshot-editor-object'))) return;
    if (event.target.closest('.cursor-pointer') && !forcePan) return;
    event.preventDefault();
    viewportRef.current.focus({ preventScroll: true });
    editor.commitText();
    const current = editor.stateRef.current;
    const displayed = freezeView();
    if (!current.editing || forcePan) {
      beginPan(event, displayed);
      return;
    }
    const point = pointAt(event);
    const inside = point[0] >= 0 && point[1] >= 0 && point[0] <= width && point[1] <= height;
    setPointerPosition(current.tool === 'erase' && inside ? point : null);
    const hit = inside && ['select', 'text', 'mosaic'].includes(current.tool) && hitObject(current.scene, point);
    if (hit) {
      editor.select(hit.id);
      capturePointer(event, { type: 'move', start: point, original: structuredClone(hit), before: cloneScene(current.scene), nodeId: hit.id });
      return;
    }
    editor.select(null);
    if (current.tool === 'select') { beginPan(event, displayed); return; }
    if (!inside) return;
    const before = cloneScene(current.scene);
    const node = { id: 'annotation-' + (++nextObjectId), kind: current.tool };
    if (['brush', 'erase'].includes(current.tool)) {
      Object.assign(node, { color: current.color, size: current.tool === 'brush' ? current.brushSize : current.eraseSize, points: [point] });
      capturePointer(event, { type: 'stroke', node, before });
      editor.publish(node.kind === 'erase' ? eraseScene(image, before, node) : [...before, node]);
    } else {
      Object.assign(node, { cx: point[0], cy: point[1], w: 0, h: 0, angle: 0, blocks: current.blocks,
        color: current.color, fontSize: current.fontSize, text: '' });
      capturePointer(event, { type: 'create', node, before, start: point });
    }
  };

  const pointerMove = (event) => {
    const gesture = gestureRef.current;
    if (!image || (gesture && gesture.pointerId !== event.pointerId)) return;
    const current = editor.stateRef.current;
    const rawPoint = pointAt(event);
    const inside = rawPoint[0] >= 0 && rawPoint[1] >= 0 && rawPoint[0] <= width && rawPoint[1] <= height;
    if (current.editing && current.tool === 'erase') setPointerPosition(inside && !event.altKey ? rawPoint : null);
    if (!gesture) {
      setHoveringObject(Boolean(image && current.editing && ['select', 'text', 'mosaic'].includes(current.tool)
        && hitObject(current.scene, rawPoint)));
      return;
    }
    if (gesture.type === 'pan') {
      const dx = event.clientX - gesture.start[0];
      const dy = event.clientY - gesture.start[1];
      if (!gesture.moved && Math.hypot(dx, dy) < PAN_THRESHOLD) return;
      gesture.moved = true;
      setPanning(true);
      setView({ ...gesture.original,
        x: clamp(gesture.original.x + dx, -width * actualScale, width * actualScale),
        y: clamp(gesture.original.y + dy, -height * actualScale, height * actualScale) });
      return;
    }
    const point = rawPoint.map((value, index) => clamp(value, 0, index ? height : width));
    if (gesture.type === 'stroke') {
      const last = gesture.node.points[gesture.node.points.length - 1];
      if (Math.hypot(point[0] - last[0], point[1] - last[1]) < 0.5 || gesture.node.points.length >= 10_000) return;
      gesture.node.points.push(point);
      editor.publish(gesture.node.kind === 'erase' ? eraseScene(image, gesture.before, gesture.node) : [...gesture.before, gesture.node]);
    } else if (gesture.type === 'create') {
      gesture.node.w = Math.abs(point[0] - gesture.start[0]);
      gesture.node.h = Math.abs(point[1] - gesture.start[1]);
      gesture.node.cx = (point[0] + gesture.start[0]) / 2;
      gesture.node.cy = (point[1] + gesture.start[1]) / 2;
      editor.publish(gesture.node.kind === 'mosaic' && gesture.node.w && gesture.node.h ? [...gesture.before, gesture.node] : gesture.before);
    } else {
      let node = structuredClone(gesture.original);
      if (gesture.type === 'move') {
        node.cx += point[0] - gesture.start[0];
        node.cy += point[1] - gesture.start[1];
      } else if (gesture.type === 'rotate') {
        const angle = Math.atan2(point[1] - node.cy, point[0] - node.cx);
        node.angle = ((node.angle + (angle - gesture.startAngle) * 180 / Math.PI + 540) % 360) - 180;
        if (event.shiftKey) node.angle = Math.round(node.angle / 15) * 15;
      } else if (gesture.type === 'resize') {
        node = resizeObject(node, point, gesture.corner, width, height, editor.measure);
      }
      constrainObject(node, width, height);
      editor.publish(gesture.before.map((item) => item.id === node.id ? node : item));
    }
  };

  const releaseGesture = useCallback(() => {
    const gesture = gestureRef.current;
    if (!gesture) return null;
    gestureRef.current = null;
    const element = viewportRef.current;
    if (element?.hasPointerCapture(gesture.pointerId)) element.releasePointerCapture(gesture.pointerId);
    setPanning(false);
    setInteracting(false);
    return gesture;
  }, []);

  const cancelGesture = useCallback(() => {
    const gesture = releaseGesture();
    if (gesture && gesture.type !== 'pan') editor.publish(gesture.before);
  }, [editor.publish, releaseGesture]);

  const finishGesture = (event, cancelled = false) => {
    const gesture = gestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    if (cancelled) { cancelGesture(); return; }
    releaseGesture();
    if (gesture.type === 'pan') return;
    if (gesture.type === 'create') {
      const node = gesture.node;
      if (node.kind === 'text') {
        const left = node.w > 6 ? node.cx - node.w / 2 : gesture.start[0];
        const top = node.h > 6 ? node.cy - node.h / 2 : gesture.start[1];
        node.wrapWidth = node.w > 6 ? node.w : width - left - 8;
        editor.fit(node);
        node.cx = left + node.w / 2;
        node.cy = top + node.h / 2;
        constrainObject(node, width, height);
        editor.publish([...gesture.before, node]);
        editor.beginText(node, gesture.before);
        return;
      }
      if (node.w < 6 || node.h < 6) { editor.publish(gesture.before); return; }
      editor.publish([...gesture.before, node]);
      editor.select(node.id);
    }
    editor.record(gesture.before);
  };

  const clearSpace = useCallback(() => {
    spacePressedRef.current = false;
    setSpacePressed(false);
  }, []);

  useEffect(() => {
    if (!editor.editing || !isAuthenticated) return undefined;
    const keyDown = (event) => {
      if (event.code !== 'Space' && event.key !== ' ') return;
      if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing || editor.stateRef.current.busy
        || document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      if (!rootRef.current.contains(event.target) || event.target.closest(EDITABLE_TARGETS)) return;
      if (gestureRef.current && gestureRef.current.type !== 'pan') return;
      spacePressedRef.current = true;
      setSpacePressed(true);
      // Buttons retain keyboard activation. A subsequent canvas drag takes
      // focus before keyup, so Space also works just after choosing a tool.
      if (!event.target.closest('button,a[href],[role="button"]')) event.preventDefault();
    };
    const keyUp = (event) => { if (event.code === 'Space' || event.key === ' ') clearSpace(); };
    document.addEventListener('keydown', keyDown);
    document.addEventListener('keyup', keyUp);
    return () => {
      document.removeEventListener('keydown', keyDown);
      document.removeEventListener('keyup', keyUp);
      clearSpace();
    };
  }, [clearSpace, editor.editing, editor.stateRef, isAuthenticated]);

  useEffect(() => {
    const blur = () => { clearSpace(); cancelGesture(); setPointerPosition(null); };
    window.addEventListener('blur', blur);
    return () => window.removeEventListener('blur', blur);
  }, [cancelGesture, clearSpace]);

  const selected = editor.selected;
  const maskKey = selected?.kind === 'text' ? JSON.stringify([selected.w, selected.h, selected.fontSize, selected.erasures]) : '';
  const textMask = useMemo(() => selected?.kind === 'text' ? textErasureMask(selected) : undefined, [maskKey]); // eslint-disable-line react-hooks/exhaustive-deps
  const bounds = selected ? objectBounds(selected) : null;
  const contextX = selected ? viewport.width / 2 + view.x + (selected.cx - width / 2) * actualScale : 0;
  const contextY = selected ? viewport.height / 2 + view.y + (selected.cy + bounds.hy - height / 2) * actualScale + 12 : 0;
  const inverseScale = 1 / actualScale;
  const showEraserPreview = editor.editing && editor.tool === 'erase' && pointerPosition
    && !editor.busy && isAuthenticated && !spacePressed && gestureRef.current?.type !== 'pan';
  const recognitionStatus = selectedDetails?.edit_ocr?.image_hash === selectedDetails?.record?.image_hash
    ? selectedDetails?.edit_ocr?.status : null;
  const recognizing = ['pending', 'processing', 'waiting_for_auth'].includes(recognitionStatus);
  const recognitionFailed = recognitionStatus === 'failed' || retryError;
  const retryRecognition = async () => {
    setRetrying(true);
    setRetryError(false);
    try { await retryEditedScreenshotOcr(selectedEvent.id, selectedDetails.record.image_hash); }
    catch { setRetryError(true); }
    finally { setRetrying(false); }
  };
  const cursor = panning ? 'grabbing' : spacePressed || !editor.editing ? 'grab'
    : hoveringObject && ['select', 'text', 'mosaic'].includes(editor.tool) ? 'move'
    : editor.tool === 'select' ? 'grab' : editor.tool === 'text' ? 'text' : 'crosshair';

  return (
    <div ref={rootRef} className="snapshot-editor absolute inset-0 z-10" data-editing={editor.editing} data-tool={editor.tool}
      data-interacting={interacting} data-pan-ready={spacePressed} data-panning={panning}>
      <div ref={viewportRef} className="snapshot-editor-viewport" tabIndex={0} role="group" aria-label={t('snapshotEditor.canvas')}
        style={{ cursor }}
        onPointerDownCapture={pointerDown} onPointerMove={pointerMove} onPointerUp={finishGesture}
        onPointerLeave={() => { setHoveringObject(false); setPointerPosition(null); }}
        onPointerCancel={(event) => finishGesture(event, true)}
        onLostPointerCapture={(event) => finishGesture(event, true)}
        onDoubleClick={(event) => {
          if (event.target.closest('[data-editor-control]') || editor.busy || spacePressedRef.current || event.altKey) return;
          const hit = editor.editing && hitObject(editor.scene, pointAt(event));
          if (hit?.kind === 'text') editor.beginText(hit);
          else if (!editor.editing) resetZoom();
        }}>
        {image ? (
          <div ref={imageRef} className="snapshot-editor-image" style={{ width, height, transition: animateView ? undefined : 'none',
            transform: 'translate(' + (viewport.width / 2 + view.x) + 'px,' + (viewport.height / 2 + view.y) + 'px) scale(' + actualScale + ') translate(-50%,-50%)' }}>
            <canvas ref={canvasRef} role="img" aria-label={t('snapshotEditor.image')} />
            {showEraserPreview && (
              <svg className="snapshot-editor-eraser-preview" width={width} height={height} aria-hidden="true">
                <circle cx={pointerPosition[0]} cy={pointerPosition[1]} r={editor.eraseSize / 2} fill="none"
                  stroke="#458ee8" strokeWidth={1.5 * inverseScale} strokeDasharray={`${4 * inverseScale} ${3 * inverseScale}`} />
              </svg>
            )}
            {!editor.editing && <InspectorOverlay boxes={overlayBoxes} metrics={{ renderWidth: width, renderHeight: height, offsetX: 0, offsetY: 0 }}
              naturalSize={{ width, height }} onBoxClick={onBoxClick} />}
            {editor.editing && selected && !editor.busy && (
              <div className="snapshot-editor-object" data-editor-control style={{ left: selected.cx - selected.w / 2,
                top: selected.cy - selected.h / 2, width: selected.w, height: selected.h,
                transform: 'rotate(' + selected.angle + 'deg)', outlineWidth: inverseScale }}
                onPointerDown={(event) => beginTransform(event, 'move')}
                onDoubleClick={(event) => {
                  event.stopPropagation();
                  if (selected.kind === 'text' && !spacePressedRef.current && !event.altKey) editor.beginText(selected);
                }}>
                {editor.editingId === selected.id && (
                  <textarea ref={textRef} className="snapshot-editor-text" aria-label={t('snapshotEditor.editText')}
                    value={selected.text} placeholder={t('snapshotEditor.placeholder')} maxLength={16_384}
                    style={{ font: '500 ' + selected.fontSize + 'px/' + TEXT_LEADING + ' ' + TEXT_FONT,
                      padding: TEXT_PADDING, color: selected.color, maskImage: textMask, maskSize: '100% 100%', maskRepeat: 'no-repeat' }}
                    onChange={(event) => editor.updateText(event.target.value)} onPointerDown={(event) => event.stopPropagation()}
                    onDoubleClick={(event) => event.stopPropagation()} />
                )}
                <span className="snapshot-editor-rotate-arm" aria-hidden="true" style={{ top: -26 * inverseScale, height: 26 * inverseScale, width: inverseScale }} />
                <button type="button" className="snapshot-editor-rotate" aria-label={t('snapshotEditor.rotate')} title={t('snapshotEditor.rotate')}
                  style={{ top: -30 * inverseScale, width: 25 * inverseScale, height: 25 * inverseScale, borderWidth: inverseScale }}
                  onPointerDown={(event) => beginTransform(event, 'rotate')}><RotateCw style={{ width: 13 * inverseScale, height: 13 * inverseScale }} aria-hidden="true" /></button>
                {['nw', 'ne', 'se', 'sw'].map((corner) => (
                  <button key={corner} type="button" className="snapshot-editor-handle" data-corner={corner}
                    aria-label={t(CORNER_LABELS[corner])} style={{ width: 26 * inverseScale, height: 26 * inverseScale }}
                    onPointerDown={(event) => beginTransform(event, 'resize', corner)}>
                    <span style={{ width: 8 * inverseScale, height: 8 * inverseScale, borderWidth: inverseScale }} />
                  </button>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-ide-muted" role="status">
            {!imageError && <Loader2 size={20} className="animate-spin" />}
            {t(imageError ? 'snapshotEditor.imageFailed' : 'snapshotPreview.loadingImage')}
          </div>
        )}
        {editor.editing && selected && !editor.editingId && !editor.busy && (
          <div data-editor-control className="snapshot-editor-object-actions preview-action-bar"
            style={{ left: clamp(contextX - 85, 4, Math.max(4, viewport.width - 174)), top: clamp(contextY, 4, Math.max(4, viewport.height - 40)) }}>
            <Button icon={Pencil} variant="ghost" size="xs" className="rounded-full border-0 text-ide-text"
              onClick={() => selected.kind === 'text' ? editor.beginText(selected) : editor.chooseTool('mosaic')}>
              {t(selected.kind === 'text' ? 'snapshotEditor.editText' : 'snapshotEditor.editMosaic')}
            </Button>
            <Button icon={Trash2} variant="ghost" size="xs" className="rounded-full border-0"
              aria-label={t('snapshotEditor.deleteObject')} title={t('snapshotEditor.deleteObject')} onClick={editor.removeSelected} />
          </div>
        )}
      </div>
      <PreviewZoomControls scale={view.scale} onZoomIn={() => changeZoom((scale) => scale * 1.5)}
        onZoomOut={() => changeZoom((scale) => scale / 1.5)} onScaleChange={changeZoom}
        onReset={resetZoom} viewportRef={rootRef} />
      {editor.dirty && <span className="snapshot-editor-dirty"><span />{t('snapshotEditor.unsaved')}</span>}
      {(editor.error || editor.savedNotice || recognizing || recognitionFailed) && (
        <div className={'snapshot-editor-notice preview-action-bar ' + (editor.error || recognitionFailed ? 'text-ide-error' : 'text-ide-text')}
          role={editor.error || recognitionFailed ? 'alert' : 'status'}>
          {recognizing ? <Loader2 size={15} className="animate-spin" aria-hidden="true" />
            : !editor.error && !recognitionFailed && <CircleCheck size={15} className="text-ide-success" aria-hidden="true" />}
          {editor.error ? t('snapshotEditor.saveFailed', { error: editor.error })
            : recognitionFailed ? t('snapshotEditor.recognitionFailed')
            : recognitionStatus === 'waiting_for_auth' ? t('snapshotEditor.waitingForUnlock')
            : recognizing ? t('snapshotEditor.savedRecognizing')
            : recognitionStatus === 'completed' ? t('snapshotEditor.recognized') : t('snapshotEditor.saved')}
          {recognitionFailed && <Button variant="ghost" size="xs" icon={RotateCw} loading={retrying}
            disabled={!isAuthenticated || editor.busy} onClick={retryRecognition}>{t('snapshotEditor.retryRecognition')}</Button>}
        </div>
      )}
      <PreviewActionBar selectedEvent={selectedEvent} selectedDetails={selectedDetails} {...actionBarProps} editor={editor} />
      <ConfirmDialog isOpen={editor.confirmLeave} title={t('snapshotEditor.leaveTitle')}
        message={editor.error ? t('snapshotEditor.saveFailed', { error: editor.error }) : t('snapshotEditor.leaveMessage')}
        confirmLabel={t('snapshotEditor.discard')} cancelLabel={t('snapshotEditor.continueEditing')}
        confirmVariant="danger" onConfirm={editor.discard} onCancel={editor.continueEditing}
        loading={editor.busy} loadingLabel={t('snapshotEditor.saving')}
        extraAction={<Button variant="primary" size="sm" loading={editor.busy} onClick={() => editor.save(true)}>{t('snapshotEditor.save')}</Button>} />
    </div>
  );
}
