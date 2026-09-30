import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useFollowOutput } from './useFollowOutput';

let notifyResize;
let height;
let viewport;
let disconnect;

function Harness({ text = 'answer', turn = 'first' }) {
  const { scrollRef, contentRef, following, onScroll, onWheel, resume, pause } = useFollowOutput(turn);
  return <>
    <div data-testid="scroll" ref={scrollRef} onScroll={onScroll} onWheel={onWheel}>
      <div ref={contentRef}>{text}</div>
    </div>
    <button onClick={resume}>Resume</button>
    <button onClick={pause}>Browse activity</button>
    <output>{following ? 'following' : 'paused'}</output>
  </>;
}

beforeEach(() => {
  height = 1000;
  viewport = 400;
  disconnect = vi.fn();
  vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(() => height);
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(() => viewport);
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback) { notifyResize = callback; }
    observe() {}
    disconnect() { disconnect(); }
  });
});

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe('output following', () => {
  it('honors an upward wheel gesture even when output arrives before the scroll event', () => {
    const { rerender } = render(<Harness />);
    const scroll = screen.getByTestId('scroll');
    fireEvent.wheel(scroll, { deltaY: -100 });
    height = 1200;
    rerender(<Harness text="a racing text delta" />);
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(600);
    expect(screen.getByText('paused')).toBeInTheDocument();
    fireEvent.scroll(scroll, { target: { scrollTop: 500 } });
    expect(scroll.scrollTop).toBe(500);
  });

  it('follows streamed text, delayed content and viewport resizing', () => {
    const { rerender, unmount } = render(<Harness />);
    const scroll = screen.getByTestId('scroll');
    expect(scroll.scrollTop).toBe(600);
    height = 1100;
    rerender(<Harness text="more output" />);
    expect(scroll.scrollTop).toBe(700);
    height = 1300;
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(900);
    viewport = 300;
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(1000);
    unmount();
    expect(disconnect).toHaveBeenCalled();
  });

  it('pauses even for a small upward scroll and resumes when the reader returns to the bottom', () => {
    const { rerender } = render(<Harness />);
    const scroll = screen.getByTestId('scroll');
    fireEvent.scroll(scroll, { target: { scrollTop: 590 } });
    expect(screen.getByText('paused')).toBeInTheDocument();
    height = 1300;
    rerender(<Harness text="continued output" />);
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(590);
    fireEvent.scroll(scroll, { target: { scrollTop: 900 } });
    expect(screen.getByText('following')).toBeInTheDocument();
    height = 1400;
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(1000);
  });

  it('resumes explicitly or for a new turn and keeps following when activity collapses', () => {
    const { rerender } = render(<Harness />);
    const scroll = screen.getByTestId('scroll');
    fireEvent.click(screen.getByText('Browse activity'));
    height = 1200;
    act(() => notifyResize());
    expect(scroll.scrollTop).toBe(600);
    fireEvent.click(screen.getByText('Resume'));
    expect(scroll.scrollTop).toBe(800);
    // Browsers clamp scrollTop before emitting a scroll event after content shrinks.
    height = 900;
    fireEvent.scroll(scroll, { target: { scrollTop: 500 } });
    expect(screen.getByText('following')).toBeInTheDocument();
    fireEvent.scroll(scroll, { target: { scrollTop: 200 } });
    height = 1500;
    rerender(<Harness turn="second" />);
    expect(scroll.scrollTop).toBe(1100);
    expect(screen.getByText('following')).toBeInTheDocument();
  });
});
