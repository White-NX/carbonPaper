import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
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
  vi.spyOn(viewport, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, width: 800, height: 600 });
  viewport.setPointerCapture = vi.fn();
  viewport.hasPointerCapture = () => true;
  viewport.releasePointerCapture = vi.fn();
  return { ...rendered, image, viewport };
}

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

  it('continues panning from the displayed view instead of jumping to a pending zoom target', async () => {
    const { image, viewport } = await renderEditor();
    fireEvent.wheel(viewport, { deltaY: -400, clientX: 400, clientY: 300 });
    vi.spyOn(image, 'getBoundingClientRect').mockReturnValue({ left: -50, top: -25, width: 1000, height: 750 });
    fireEvent.pointerDown(viewport, { button: 0, clientX: 200, clientY: 125, pointerId: 1 });
    fireEvent.pointerMove(viewport, { clientX: 300, clientY: 175, pointerId: 1 });
    expect(image.style.transform).toBe('translate(550px,400px) scale(1.25) translate(-50%,-50%)');
    fireEvent.pointerUp(viewport, { button: 0, clientX: 300, clientY: 175, pointerId: 1 });
    expect(viewport.releasePointerCapture).toHaveBeenCalledWith(1);
  });
});
