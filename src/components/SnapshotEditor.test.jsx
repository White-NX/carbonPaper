import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import SnapshotEditor from './SnapshotEditor';
import { renderScene } from '../lib/snapshot_editor';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../lib/snapshot_editor', async (importOriginal) => ({ ...(await importOriginal()), renderScene: vi.fn() }));

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  vi.stubGlobal('matchMedia', () => ({ matches: false }));
  vi.stubGlobal('Image', class {
    naturalWidth = 800;
    naturalHeight = 600;
    set src(_value) { queueMicrotask(() => this.onload?.()); }
  });
  vi.stubGlobal('PointerEvent', class extends MouseEvent {
    constructor(type, options) { super(type, options); this.pointerId = options.pointerId || 1; }
  });
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(600);
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
    measureText: (text) => ({ width: [...text].length * 12 }), strokeRect: vi.fn(), setLineDash: vi.fn(),
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function renderEditor() {
  let rendered;
  await act(async () => { rendered = render(<SnapshotEditor imageSrc="preview.png" selectedEvent={{ id: 42 }}
    selectedDetails={{ record: { id: 42, status: 'committed', image_hash: 'original' } }}
    isAuthenticated actionBarProps={{}} />); });
  const image = screen.getByRole('img').parentElement;
  const viewport = image.parentElement;
  vi.spyOn(viewport, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, width: viewport.clientWidth, height: viewport.clientHeight });
  vi.spyOn(image, 'getBoundingClientRect').mockReturnValue({ left: (viewport.clientWidth - 800) / 2,
    top: (viewport.clientHeight - 600) / 2, width: 800, height: 600 });
  viewport.setPointerCapture = vi.fn();
  viewport.hasPointerCapture = () => true;
  viewport.releasePointerCapture = vi.fn();
  return { ...rendered, image, viewport };
}

function createMosaic(viewport) {
  fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
  fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.mosaic' }));
  fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 120, clientY: 150, pointerId: 1 });
  fireEvent.pointerMove(viewport, { clientX: 200, clientY: 210, pointerId: 1 });
  fireEvent.pointerUp(viewport, { clientX: 200, clientY: 210, pointerId: 1 });
  return viewport.querySelector('.snapshot-editor-object');
}

const lastScene = () => structuredClone(renderScene.mock.calls.at(-1)[2]);

describe('snapshot editor zoom and pointer handoff', () => {
  it('shows a newly loaded snapshot at 100% without animating from the previous zoom', async () => {
    const { image, viewport, rerender } = await renderEditor();
    expect(image.style.transition).toBe('none');
    fireEvent.wheel(viewport, { deltaY: -400, clientX: 400, clientY: 300 });
    expect(image.style.transition).toBe('');
    expect(screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 182%' })).toBeInTheDocument();

    await act(async () => rerender(<SnapshotEditor imageSrc="next.png" selectedEvent={{ id: 43 }}
      selectedDetails={{ record: { id: 43, status: 'committed', image_hash: 'next' } }}
      isAuthenticated actionBarProps={{}} />));
    expect(screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 100%' })).toBeInTheDocument();
    expect(screen.getByRole('img').parentElement.style.transition).toBe('none');
    expect(screen.getByRole('img').parentElement.style.transform).toBe('translate(400px,300px) scale(1) translate(-50%,-50%)');
  });

  it('zooms around the pointer without rasterizing the unchanged screenshot again', async () => {
    const { viewport } = await renderEditor();
    const renders = renderScene.mock.calls.length;
    fireEvent.wheel(viewport, { deltaY: -100, clientX: 400, clientY: 300 });
    expect(screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 116%' })).toBeInTheDocument();
    expect(renderScene).toHaveBeenCalledTimes(renders);

    fireEvent.wheel(viewport, { deltaY: -3, deltaMode: 1, clientX: 400, clientY: 300 });
    expect(screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 125%' })).toBeInTheDocument();
  });

  it('starts a stroke at the displayed image coordinates while a zoom is still moving', async () => {
    const { image, viewport } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.brush' }));
    fireEvent.wheel(viewport, { deltaY: -400, clientX: 400, clientY: 300 });
    // The image has only reached 125%, although the requested zoom is 182%.
    vi.spyOn(image, 'getBoundingClientRect').mockReturnValue({ left: -50, top: -25, width: 1000, height: 750 });
    fireEvent.pointerDown(viewport, { button: 0, clientX: 200, clientY: 125, pointerId: 1 });
    expect(renderScene.mock.calls.at(-1)[2][0]).toMatchObject({ kind: 'brush', points: [[200, 120]] });
    expect(screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 125%' })).toBeInTheDocument();
    fireEvent.pointerUp(viewport, { button: 0, clientX: 200, clientY: 125, pointerId: 1 });
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeEnabled();
  });

  it.each([false, true])('continues panning from the displayed view instead of jumping to a pending zoom target (editing: %s)', async (editing) => {
    const { image, viewport } = await renderEditor();
    if (editing) fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.wheel(viewport, { deltaY: -400, clientX: 400, clientY: 300 });
    vi.spyOn(image, 'getBoundingClientRect').mockReturnValue({ left: -50, top: -25, width: 1000, height: 750 });
    fireEvent.pointerDown(viewport, { button: 0, clientX: 200, clientY: 125, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 300, clientY: 175, pointerId: 1 });
    expect(image.style.transform).toBe('translate(550px,400px) scale(1.25) translate(-50%,-50%)');
    fireEvent.pointerUp(viewport, { button: 0, clientX: 300, clientY: 175, pointerId: 1 });
    expect(viewport.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it('pans from the surrounding blank space in selection mode only after a drag, without changing the draft', async () => {
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(1200);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(900);
    const { image, viewport } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    expect(viewport.style.cursor).toBe('grab');

    fireEvent.pointerDown(viewport, { button: 0, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 102, clientY: 101, pointerId: 1 });
    expect(image.style.transform).toBe('translate(600px,450px) scale(1) translate(-50%,-50%)');
    expect(viewport.style.cursor).toBe('grab');
    fireEvent.pointerMove(viewport, { clientX: 180, clientY: 140, pointerId: 1 });
    expect(image.style.transform).toBe('translate(680px,490px) scale(1) translate(-50%,-50%)');
    expect(viewport.style.cursor).toBe('grabbing');
    fireEvent.pointerUp(viewport, { clientX: 180, clientY: 140, pointerId: 1 });
    expect(viewport.style.cursor).toBe('grab');
    expect(screen.queryByText('snapshotEditor.unsaved')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeDisabled();
  });
});

describe('snapshot editor canvas and annotation dragging', () => {
  it('moves a hit annotation instead of panning the canvas in selection mode', async () => {
    const { image, viewport } = await renderEditor();
    createMosaic(viewport);
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.select' }));
    fireEvent.pointerMove(screen.getByRole('img'), { clientX: 160, clientY: 180, pointerId: 1 });
    expect(viewport.style.cursor).toBe('move');
    fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 160, clientY: 180, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 200, clientY: 200, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 200, clientY: 200, pointerId: 1 });
    expect(lastScene()[0]).toMatchObject({ kind: 'mosaic', cx: 200, cy: 200, w: 80, h: 60 });
    expect(image.style.transform).toBe('translate(400px,300px) scale(1) translate(-50%,-50%)');
  });

  it('temporarily pans with Space after choosing the brush, then draws at the moved image coordinates', async () => {
    const { image, viewport } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    const brush = screen.getByRole('button', { name: 'snapshotEditor.tools.brush' });
    fireEvent.click(brush);
    expect(fireEvent.keyDown(brush, { code: 'Space', key: ' ' })).toBe(true);
    expect(viewport.style.cursor).toBe('grab');
    fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 140, clientY: 120, pointerId: 1 });
    fireEvent.keyUp(viewport, { code: 'Space', key: ' ' });
    fireEvent.pointerMove(viewport, { clientX: 180, clientY: 140, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 180, clientY: 140, pointerId: 1 });
    expect(image.style.transform).toBe('translate(480px,340px) scale(1) translate(-50%,-50%)');
    expect(lastScene()).toEqual([]);
    expect(screen.queryByText('snapshotEditor.unsaved')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeDisabled();
    expect(brush).toHaveAttribute('aria-pressed', 'true');
    expect(viewport.style.cursor).toBe('crosshair');

    vi.spyOn(image, 'getBoundingClientRect').mockReturnValue({ left: 80, top: 40, width: 800, height: 600 });
    fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 120, clientY: 120, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 120, clientY: 120, pointerId: 1 });
    expect(lastScene()[0]).toMatchObject({ kind: 'brush', points: [[20, 60], [40, 80]] });
    expect(image.style.transform).toBe('translate(480px,340px) scale(1) translate(-50%,-50%)');
  });

  it('gives Space panning priority over a selected annotation and its resize handles without adding history', async () => {
    const { image, viewport } = await renderEditor();
    const object = createMosaic(viewport);
    const before = lastScene();
    fireEvent.keyDown(viewport, { code: 'Space', key: ' ' });
    fireEvent.pointerDown(object.querySelector('[data-corner="nw"]'), { button: 0, clientX: 120, clientY: 150, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 220, clientY: 200, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 220, clientY: 200, pointerId: 1 });
    fireEvent.keyUp(viewport, { code: 'Space', key: ' ' });
    expect(image.style.transform).toBe('translate(500px,350px) scale(1) translate(-50%,-50%)');
    expect(lastScene()).toEqual(before);
    expect(screen.getByRole('button', { name: 'snapshotEditor.tools.mosaic' })).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.undo' }));
    expect(lastScene()).toEqual([]);
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeDisabled();
  });

  it.each(['middle', 'alt'])('also pans over a selected annotation with %s dragging', async (modifier) => {
    const { image, viewport } = await renderEditor();
    const object = createMosaic(viewport);
    const before = lastScene();
    fireEvent.pointerDown(object, { button: modifier === 'middle' ? 1 : 0, altKey: modifier === 'alt',
      clientX: 160, clientY: 180, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 240, clientY: 220, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 240, clientY: 220, pointerId: 1 });
    expect(image.style.transform).toBe('translate(480px,340px) scale(1) translate(-50%,-50%)');
    expect(lastScene()).toEqual(before);
  });

  it('keeps spaces and pointer interaction in the inline text input', async () => {
    const user = userEvent.setup();
    const { image, viewport } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.text' }));
    fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerUp(viewport, { clientX: 100, clientY: 100, pointerId: 1 });
    const input = screen.getByRole('textbox', { name: 'snapshotEditor.editText' });
    const captures = viewport.setPointerCapture.mock.calls.length;
    await user.type(input, 'hello world');
    expect(input).toHaveValue('hello world');
    expect(input).toHaveFocus();
    expect(viewport.setPointerCapture).toHaveBeenCalledTimes(captures);
    expect(viewport.parentElement).toHaveAttribute('data-pan-ready', 'false');
    expect(image.style.transform).toBe('translate(400px,300px) scale(1) translate(-50%,-50%)');
  });

  it.each(['blur', 'lostpointercapture'])('ends a temporary pan on %s and stops responding to the old pointer', async (interruption) => {
    const { image, viewport } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.brush' }));
    fireEvent.keyDown(viewport, { code: 'Space', key: ' ' });
    fireEvent.pointerDown(screen.getByRole('img'), { button: 0, clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 180, clientY: 140, pointerId: 1 });
    if (interruption === 'blur') {
      fireEvent(window, new Event('blur'));
      expect(viewport.style.cursor).toBe('crosshair');
    } else {
      fireEvent.lostPointerCapture(viewport, { pointerId: 1 });
      fireEvent.keyUp(document.body, { code: 'Space', key: ' ' });
    }
    fireEvent.pointerMove(viewport, { clientX: 300, clientY: 250, pointerId: 1 });
    expect(image.style.transform).toBe('translate(480px,340px) scale(1) translate(-50%,-50%)');
    expect(viewport.style.cursor).toBe('crosshair');
    expect(viewport.releasePointerCapture).toHaveBeenCalledWith(1);
    expect(screen.queryByText('snapshotEditor.unsaved')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeDisabled();
  });
});
