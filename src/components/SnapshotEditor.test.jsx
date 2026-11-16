import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import SnapshotEditor from './SnapshotEditor';
import { renderScene } from '../lib/snapshot_editor';
import { saveEditedScreenshot, retryEditedScreenshotOcr } from '../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../lib/snapshot_editor', async (importOriginal) => ({ ...(await importOriginal()), renderScene: vi.fn() }));
vi.mock('../lib/monitor_api', async (importOriginal) => ({
  ...(await importOriginal()), saveEditedScreenshot: vi.fn(), retryEditedScreenshotOcr: vi.fn(),
}));

beforeEach(() => {
  saveEditedScreenshot.mockReset();
  retryEditedScreenshotOcr.mockReset();
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
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function renderEditor(overrides = {}) {
  let rendered;
  await act(async () => { rendered = render(<SnapshotEditor imageSrc="preview.png" selectedEvent={{ id: 42 }}
    selectedDetails={{ record: { id: 42, status: 'committed', image_hash: 'original' } }}
    isAuthenticated actionBarProps={{}} {...overrides} />); });
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

describe('snapshot editor background recognition', () => {
  it('dismisses a successful save notice after a short delay', async () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'toDataURL').mockReturnValue('data:image/png;base64,cG5n');
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 42, image_hash: 'saved' });
    const { viewport } = await renderEditor();
    vi.useFakeTimers();
    createMosaic(viewport);
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.save' })));
    expect(screen.getByText('snapshotEditor.saved')).toBeInTheDocument();
    await act(async () => vi.advanceTimersByTimeAsync(2000));
    expect(screen.queryByText('snapshotEditor.saved')).not.toBeInTheDocument();
  });

  it('keeps pending and failed recognition visible but dismisses completion without extending it on detail refresh', async () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'toDataURL').mockReturnValue('data:image/png;base64,cG5n');
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 42, image_hash: 'saved', ocr_status: 'pending' });
    let view;
    const renderStatus = (status) => view.rerender(<SnapshotEditor imageSrc="saved.png" selectedEvent={{ id: 42 }}
      selectedDetails={{ record: { id: 42, status: 'committed', image_hash: 'saved' },
        edit_ocr: { screenshot_id: 42, image_hash: 'saved', status } }}
      isAuthenticated actionBarProps={{}} onSaved={onSaved} />);
    const onSaved = () => renderStatus('pending');
    view = await renderEditor({ onSaved });
    vi.useFakeTimers();
    createMosaic(view.viewport);
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.save' })));
    await act(async () => vi.advanceTimersByTimeAsync(10000));
    expect(screen.getByText('snapshotEditor.savedRecognizing')).toBeInTheDocument();
    await act(async () => renderStatus('failed'));
    await act(async () => vi.advanceTimersByTimeAsync(10000));
    expect(screen.getByText('snapshotEditor.recognitionFailed')).toBeInTheDocument();
    await act(async () => renderStatus('completed'));
    expect(screen.getByText('snapshotEditor.recognized')).toBeInTheDocument();
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    await act(async () => renderStatus('completed'));
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(screen.queryByText('snapshotEditor.recognized')).not.toBeInTheDocument();
  });

  it('closes editing after saving while the surrounding refresh is still pending', async () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'toDataURL').mockReturnValue('data:image/png;base64,cG5n');
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 42, image_hash: 'saved', ocr_status: 'pending' });
    const onSaved = vi.fn(() => new Promise(() => {}));
    const { viewport } = await renderEditor({ onSaved });
    createMosaic(viewport);
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.save' })));
    expect(viewport.parentElement).toHaveAttribute('data-editing', 'false');
    expect(screen.getByRole('button', { name: 'snapshotEditor.edit' })).toBeEnabled();
    expect(saveEditedScreenshot).toHaveBeenCalledWith(42, 'original', 'cG5n');
    expect(onSaved).toHaveBeenCalledWith(expect.objectContaining({ ocr_status: 'pending' }), 'data:image/png;base64,cG5n');
  });

  it('allows another edit while recognition retry is in flight', async () => {
    let finish;
    retryEditedScreenshotOcr.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const { viewport } = await renderEditor({ selectedDetails: {
      record: { id: 42, status: 'committed', image_hash: 'original' },
      edit_ocr: { screenshot_id: 42, image_hash: 'original', status: 'failed' },
    } });
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.retryRecognition' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    expect(viewport.parentElement).toHaveAttribute('data-editing', 'true');
    expect(screen.getByRole('button', { name: 'snapshotEditor.tools.brush' })).toBeEnabled();
    expect(retryEditedScreenshotOcr).toHaveBeenCalledWith(42, 'original');
    await act(async () => finish({ status: 'pending' }));
  });
});

describe('snapshot editor canvas and annotation dragging', () => {
  it('previews the eraser diameter at the pointer and updates its range without changing the image', async () => {
    const { viewport, image } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.erase' }));
    const renders = renderScene.mock.calls.length;
    fireEvent.pointerMove(viewport, { clientX: 160, clientY: 180, pointerId: 1 });
    const circle = image.querySelector('.snapshot-editor-eraser-preview circle');
    expect(circle).toHaveAttribute('cx', '160');
    expect(circle).toHaveAttribute('cy', '180');
    expect(circle).toHaveAttribute('r', '14');
    expect(circle).toHaveAttribute('stroke', '#458ee8');
    expect(circle).toHaveAttribute('stroke-dasharray', '4 3');
    expect(circle.parentElement).toHaveAttribute('aria-hidden', 'true');
    expect(renderScene).toHaveBeenCalledTimes(renders);

    fireEvent.change(screen.getByRole('slider', { name: 'snapshotEditor.eraseRange' }), { target: { value: '60' } });
    expect(circle).toHaveAttribute('r', '30');
    fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.zoomIn' }));
    expect(image.style.transform).toContain('scale(1.5)');
    expect(circle).toHaveAttribute('r', '30');
    expect(circle).toHaveAttribute('stroke-width', '1');
    expect(lastScene()).toEqual([]);
    expect(screen.queryByText('snapshotEditor.unsaved')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'snapshotEditor.undo' })).toBeDisabled();
  });

  it('hides the eraser preview outside the image and during temporary canvas panning', async () => {
    const { viewport, image } = await renderEditor();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.tools.erase' }));
    const preview = () => image.querySelector('.snapshot-editor-eraser-preview');
    fireEvent.pointerMove(viewport, { clientX: 160, clientY: 180, pointerId: 1 });
    expect(preview()).not.toBeNull();
    fireEvent.keyDown(viewport, { code: 'Space', key: ' ' });
    expect(preview()).toBeNull();
    fireEvent.keyUp(viewport, { code: 'Space', key: ' ' });
    expect(preview()).not.toBeNull();
    fireEvent.pointerDown(viewport, { button: 1, clientX: 160, clientY: 180, pointerId: 1 });
    expect(preview()).toBeNull();
    fireEvent.pointerUp(viewport, { button: 1, clientX: 160, clientY: 180, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 900, clientY: 180, pointerId: 1 });
    expect(preview()).toBeNull();
    fireEvent.pointerMove(viewport, { clientX: 160, clientY: 180, pointerId: 1 });
    fireEvent.pointerLeave(viewport);
    expect(preview()).toBeNull();
  });

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
