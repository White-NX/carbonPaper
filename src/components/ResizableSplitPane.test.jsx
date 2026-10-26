import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import ResizableSplitPane from './ResizableSplitPane';

let availableWidth;
let resize;
const pane = (active = true) => <ResizableSplitPane storageKey="testListWidth" separatorLabel="Resize list" left={<span>List</span>} active={active}><span>Details</span></ResizableSplitPane>;

beforeEach(() => {
  localStorage.clear();
  availableWidth = 1000;
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(() => availableWidth);
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback) { resize = callback; }
    observe() {}
    disconnect() {}
  });
  vi.stubGlobal('PointerEvent', class extends MouseEvent {
    constructor(type, options = {}) {
      super(type, options);
      this.pointerId = options.pointerId ?? 1;
    }
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function captureHandle() {
  const handle = screen.getByRole('separator', { name: 'Resize list' });
  handle.setPointerCapture = vi.fn();
  handle.hasPointerCapture = () => true;
  handle.releasePointerCapture = vi.fn();
  return handle;
}

it('bounds a saved list width to the window and restores it when space becomes available', () => {
  localStorage.setItem('testListWidth', '400');
  availableWidth = 600;
  render(pane());
  const handle = screen.getByRole('separator');
  expect(handle).toHaveAttribute('aria-valuenow', '276');
  expect(localStorage.getItem('testListWidth')).toBe('400');
  availableWidth = 1000;
  act(() => resize());
  expect(handle).toHaveAttribute('aria-valuenow', '400');
});

it('saves completed drags and rolls back cancelled drags', () => {
  render(pane());
  const handle = captureHandle();
  fireEvent.pointerDown(handle, { button: 0, clientX: 280, pointerId: 1 });
  fireEvent.pointerMove(handle, { clientX: 400, pointerId: 1 });
  expect(handle).toHaveAttribute('aria-valuenow', '400');
  expect(localStorage.getItem('testListWidth')).toBeNull();
  fireEvent.pointerUp(handle, { pointerId: 1 });
  expect(localStorage.getItem('testListWidth')).toBe('400');
  fireEvent.pointerDown(handle, { button: 0, clientX: 400, pointerId: 2 });
  fireEvent.pointerMove(handle, { clientX: 50, pointerId: 2 });
  expect(handle).toHaveAttribute('aria-valuenow', '220');
  fireEvent.pointerCancel(handle, { pointerId: 2 });
  expect(handle).toHaveAttribute('aria-valuenow', '400');
  expect(localStorage.getItem('testListWidth')).toBe('400');
});

it('supports bounded keyboard resizing and resetting the default width', () => {
  const view = render(pane());
  const handle = screen.getByRole('separator');
  fireEvent.keyDown(handle, { key: 'ArrowLeft' });
  expect(handle).toHaveAttribute('aria-valuenow', '264');
  fireEvent.keyDown(handle, { key: 'End' });
  expect(handle).toHaveAttribute('aria-valuenow', '420');
  fireEvent.keyDown(handle, { key: 'Home' });
  expect(handle).toHaveAttribute('aria-valuenow', '220');
  fireEvent.doubleClick(handle);
  expect(localStorage.getItem('testListWidth')).toBe('280');
  view.unmount();
  render(pane());
  expect(screen.getByRole('separator')).toHaveAttribute('aria-valuenow', '280');
});

it('cancels pointer capture when the page becomes inactive', () => {
  const view = render(pane());
  const handle = captureHandle();
  fireEvent.pointerDown(handle, { button: 0, clientX: 280, pointerId: 1 });
  fireEvent.pointerMove(handle, { clientX: 360, pointerId: 1 });
  view.rerender(pane(false));
  expect(handle.releasePointerCapture).toHaveBeenCalledWith(1);
  expect(handle).toHaveAttribute('aria-valuenow', '280');
  expect(localStorage.getItem('testListWidth')).toBeNull();
});
