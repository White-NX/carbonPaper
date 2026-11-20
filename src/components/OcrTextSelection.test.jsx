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
  return <div ref={ref}><OcrTextSelection key={identity} results={results}
    naturalSize={{ width: 200, height: 100 }} controlsRef={ref} onSelectingChange={onSelectingChange} />
    <input aria-label="other text" defaultValue="native text" /></div>;
}

function setup(props = {}) {
  const view = render(<Harness {...props} />);
  const overlay = screen.getByRole('group', { name: 'snapshotPreview.selectText' });
  vi.spyOn(overlay, 'getBoundingClientRect').mockReturnValue({ left: 100, top: 50, width: 400, height: 200 });
  overlay.setPointerCapture = vi.fn();
  overlay.hasPointerCapture = () => true;
  overlay.releasePointerCapture = vi.fn();
  return { ...view, overlay };
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
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.copied' })));
    expect(writeText).toHaveBeenCalledTimes(2);
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
    expect(screen.queryByRole('button', { name: 'snapshotPreview.copySelectedText' })).not.toBeInTheDocument();
  });

  it('clears the selection and pending copy controls when switching screenshots', () => {
    const { overlay, rerender } = setup();
    drag(overlay);
    expect(screen.getByRole('button', { name: 'snapshotPreview.copySelectedText' })).toBeInTheDocument();
    rerender(<Harness identity={2} />);
    expect(screen.queryByRole('button', { name: 'snapshotPreview.copySelectedText' })).not.toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'snapshotPreview.selectText' }).querySelector('[data-ocr-highlight]')).toBeNull();
  });
});
