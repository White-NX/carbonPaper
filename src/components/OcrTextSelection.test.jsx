import React, { useRef } from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import OcrTextSelection from './OcrTextSelection';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

const alignedRow = (text, index = 0) => ({ id: index + 1, text,
  text_layout: { matrix: [[100, 0, 10], [0, 20, index ? 50 : 10], [0, 0, 1]],
    source_size: [200, 100], input_width: text.length * 4, valid_width: text.length * 4,
    time_steps: text.length, centers: Array.from(text, (_, i) => 2 * i + 1),
    text_lengths: Array.from(text, (character) => new TextEncoder().encode(character).length) } });
const rows = ['你好世界', 'next line'].map(alignedRow);
let writeText;

beforeEach(() => {
  writeText = vi.fn().mockResolvedValue();
  vi.stubGlobal('navigator', { clipboard: { writeText } });
  vi.stubGlobal('PointerEvent', class extends MouseEvent {
    constructor(type, options = {}) { super(type, options); this.pointerId = options.pointerId || 1; }
  });
});
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function Harness({ identity = 1, results = rows, onSelectingChange }) {
  const ref = useRef(null);
  return <div ref={ref} data-testid="viewport"><OcrTextSelection key={identity} results={results}
    naturalSize={{ width: 200, height: 100 }} controlsRef={ref} onSelectingChange={onSelectingChange} />
    <input aria-label="other text" defaultValue="native text" /></div>;
}

function setup(props = {}) {
  const view = render(<Harness {...props} />);
  const overlay = screen.getByRole('group', { name: 'snapshotPreview.selectText' });
  const viewport = screen.getByTestId('viewport');
  vi.spyOn(viewport, 'getBoundingClientRect').mockReturnValue({ left: 100, top: 50, width: 400, height: 200 });
  vi.spyOn(overlay, 'getBoundingClientRect').mockReturnValue({ left: 100, top: 50, width: 400, height: 200 });
  overlay.setPointerCapture = vi.fn();
  overlay.hasPointerCapture = () => true;
  overlay.releasePointerCapture = vi.fn();
  return { ...view, overlay, viewport };
}

function drag(overlay, reverse = false) {
  const start = reverse ? { clientX: 209, clientY: 170 } : { clientX: 170, clientY: 90 };
  const end = reverse ? { clientX: 170, clientY: 90 } : { clientX: 209, clientY: 170 };
  fireEvent.pointerDown(overlay.querySelector(`[data-ocr-line="${reverse ? 1 : 0}"]`), { button: 0, pointerId: 3, ...start });
  fireEvent.pointerMove(overlay, { pointerId: 3, ...end });
  fireEvent.pointerUp(overlay, { pointerId: 3, ...end });
}

describe('screenshot text selection interactions', () => {
  it('double-clicks the word under the pointer on the right half of its last letter', async () => {
    const { overlay } = setup({ results: [alignedRow('hello world')] });
    const lastLetter = { button: 0, clientX: 206, clientY: 90 };
    const line = overlay.querySelector('[data-ocr-line="0"]');
    fireEvent.pointerDown(line, lastLetter);
    fireEvent.pointerUp(overlay, lastLetter);
    fireEvent.doubleClick(line, lastLetter);
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('hello');
  });

  it.each([false, true])('copies a cross-line drag at a scaled image size (reverse: %s)', async (reverse) => {
    const selecting = vi.fn();
    const { overlay } = setup({ onSelectingChange: selecting });
    drag(overlay, reverse);
    expect(overlay.querySelectorAll('[data-ocr-highlight]')).toHaveLength(2);
    expect(selecting.mock.calls).toEqual([[true], [false]]);
    expect(overlay.releasePointerCapture).toHaveBeenCalledWith(3);
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('好世界\nnext');
    expect(screen.getByRole('status')).toHaveTextContent('snapshotPreview.copied');
    fireEvent.contextMenu(overlay, { clientX: 210, clientY: 170 });
    await act(async () => fireEvent.click(screen.getByRole('menuitem', { name: 'snapshotPreview.copy' })));
    expect(writeText).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  });

  it('keeps native input shortcuts separate and supports select-all and Escape within the screenshot', async () => {
    const { overlay } = setup();
    drag(overlay);
    const input = screen.getByRole('textbox', { name: 'other text' });
    input.focus();
    const nativeCopy = new KeyboardEvent('keydown', { key: 'c', ctrlKey: true, bubbles: true, cancelable: true });
    fireEvent(input, nativeCopy);
    expect(nativeCopy.defaultPrevented).toBe(false);
    expect(writeText).not.toHaveBeenCalled();
    overlay.focus();
    fireEvent.keyDown(overlay, { key: 'a', ctrlKey: true });
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('你好世界\nnext line');
    fireEvent.keyDown(overlay, { key: 'Escape' });
    expect(overlay.querySelector('[data-ocr-highlight]')).toBeNull();
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  });

  it('clears the selection and pending copy feedback when switching screenshots', async () => {
    let finishCopy;
    writeText.mockImplementation(() => new Promise((resolve) => { finishCopy = resolve; }));
    const { overlay, rerender } = setup();
    drag(overlay);
    fireEvent.contextMenu(overlay, { clientX: 210, clientY: 170 });
    expect(screen.getByRole('menuitem', { name: 'snapshotPreview.copy' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('menuitem', { name: 'snapshotPreview.copy' }));
    rerender(<Harness identity={2} />);
    await act(async () => finishCopy());
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'snapshotPreview.selectText' }).querySelector('[data-ocr-highlight]')).toBeNull();
  });

  it('preserves selected text on right-click and menu interaction, with Escape closing the menu first', async () => {
    const { overlay, viewport } = setup();
    drag(overlay);
    expect(screen.queryByRole('button', { name: 'snapshotPreview.copySelectedText' })).not.toBeInTheDocument();
    fireEvent.pointerDown(viewport, { button: 2, clientX: 420, clientY: 200 });
    const context = new MouseEvent('contextmenu', { clientX: 420, clientY: 200, bubbles: true, cancelable: true });
    fireEvent(viewport, context);
    expect(context.defaultPrevented).toBe(true);
    const option = screen.getByRole('menuitem', { name: 'snapshotPreview.copy' });
    expect(option).toHaveFocus();
    expect(overlay.querySelectorAll('[data-ocr-highlight]')).toHaveLength(2);
    fireEvent.keyDown(option, { key: 'Escape' });
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(overlay).toHaveFocus();
    expect(overlay.querySelectorAll('[data-ocr-highlight]')).toHaveLength(2);
    fireEvent.keyDown(overlay, { key: 'F10', shiftKey: true });
    const copyOption = screen.getByRole('menuitem', { name: 'snapshotPreview.copy' });
    fireEvent.pointerDown(copyOption, { button: 0 });
    await act(async () => fireEvent.click(copyOption));
    expect(writeText).toHaveBeenLastCalledWith('好世界\nnext');
    fireEvent.keyDown(overlay, { key: 'ContextMenu' });
    fireEvent.wheel(viewport, { deltaY: 10 });
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(overlay).toHaveFocus();
    fireEvent.keyDown(overlay, { key: 'Escape' });
    expect(overlay.querySelector('[data-ocr-highlight]')).toBeNull();
  });

  it('fits the context menu inside the unscaled viewport and leaves native input menus alone', () => {
    const { overlay } = setup();
    drag(overlay);
    const measure = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
      return this.hasAttribute('data-ocr-selection-menu')
        ? { width: 192, height: 48, left: 0, top: 0 } : measure.call(this);
    });
    fireEvent.contextMenu(overlay, { clientX: 495, clientY: 245 });
    const menu = screen.getByRole('menu').closest('[data-ocr-selection-menu]');
    expect(menu.style.left).toBe('200px');
    expect(menu.style.top).toBe('144px');
    const input = screen.getByRole('textbox', { name: 'other text' });
    const context = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
    fireEvent(input, context);
    expect(context.defaultPrevented).toBe(false);
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  });

  it('keeps the selection available for retry after a clipboard failure', async () => {
    const { overlay } = setup();
    drag(overlay);
    writeText.mockRejectedValueOnce(new Error('Clipboard unavailable'));
    fireEvent.contextMenu(overlay, { clientX: 210, clientY: 170 });
    await act(async () => fireEvent.click(screen.getByRole('menuitem', { name: 'snapshotPreview.copy' })));
    expect(screen.getByRole('alert')).toHaveTextContent('snapshotPreview.copyFailed');
    expect(overlay).toHaveFocus();
    expect(overlay.querySelectorAll('[data-ocr-highlight]')).toHaveLength(2);
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('好世界\nnext');
    expect(screen.getByRole('status')).toHaveTextContent('snapshotPreview.copied');
  });

  it('keeps dragging in the body region and lets Ctrl+A explicitly include the sidebar', async () => {
    const result = (id, text, x, y, width) => ({ id, text,
      box_coords: [[x, y], [x + width, y], [x + width, y + 20], [x, y + 20]] });
    const { overlay } = setup({ results: [
      result('first', 'body first', 80, 10, 100), result('menu', 'menu', 5, 30, 30),
      result('last', 'body last', 80, 50, 100),
    ] });
    const line = overlay.querySelector('[data-ocr-line="0"]');
    fireEvent.pointerDown(line, { button: 0, pointerId: 3, clientX: 260, clientY: 85 });
    fireEvent.pointerMove(overlay, { pointerId: 3, clientX: 110, clientY: 240 });
    fireEvent.pointerUp(overlay, { pointerId: 3, clientX: 110, clientY: 240 });
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('body first\nbody last');
    const menuLine = overlay.querySelector('[data-ocr-line="1"]');
    fireEvent.pointerDown(menuLine, { button: 0, pointerId: 3, shiftKey: true, clientX: 110, clientY: 130 });
    fireEvent.pointerUp(overlay, { pointerId: 3, clientX: 170, clientY: 130 });
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('menu');
    fireEvent.keyDown(overlay, { key: 'a', ctrlKey: true });
    await act(async () => fireEvent.keyDown(overlay, { key: 'c', ctrlKey: true }));
    expect(writeText).toHaveBeenLastCalledWith('body first\nmenu\nbody last');
  });
});
