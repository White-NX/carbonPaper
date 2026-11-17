import React, { useId, useLayoutEffect, useRef } from 'react';

const ROW_HEIGHT = 32;

/** A scrollable local-time column; scrolling changes only the picker draft. */
export function TimeWheel({ label, value, max, onChange }) {
  const id = useId();
  const list = useRef(null);
  const scrollingValue = useRef(null);
  const drag = useRef(null);
  const suppressClick = useRef(false);

  useLayoutEffect(() => {
    if (scrollingValue.current === value) {
      scrollingValue.current = null;
      return;
    }
    if (list.current) list.current.scrollTop = value * ROW_HEIGHT;
  }, [value]);

  const choose = (next) => {
    const bounded = Math.max(0, Math.min(max, next));
    scrollingValue.current = null;
    list.current.scrollTop = bounded * ROW_HEIGHT;
    onChange(bounded);
  };
  const finishDrag = (event) => {
    if (!drag.current || drag.current.pointerId !== event.pointerId) return;
    if (drag.current.moved) {
      suppressClick.current = true;
      choose(Math.round(list.current.scrollTop / ROW_HEIGHT));
      if (event.currentTarget.hasPointerCapture?.(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    }
    drag.current = null;
  };

  return <div className="min-w-0">
    <div id={id + '-label'} className="mb-3 flex h-8 items-center justify-center text-xs text-ide-muted">{label}</div>
    <div className="relative rounded-lg">
      <div aria-hidden="true" className="pointer-events-none absolute inset-x-0 top-1/2 h-8 -translate-y-1/2 rounded-md bg-ide-accent/10" />
      <div ref={list} role="listbox" aria-labelledby={id + '-label'} aria-activedescendant={id + '-' + value}
        tabIndex={0} className="date-time-wheel relative rounded-lg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ide-accent/60"
        onScroll={(event) => {
          const next = Math.max(0, Math.min(max, Math.round(event.currentTarget.scrollTop / ROW_HEIGHT)));
          if (next !== value) {
            scrollingValue.current = next;
            onChange(next);
          }
        }}
        onKeyDown={(event) => {
          const next = { ArrowUp: value - 1, ArrowDown: value + 1, PageUp: value - 5, PageDown: value + 5, Home: 0, End: max };
          if (!(event.key in next)) return;
          event.preventDefault();
          event.stopPropagation();
          choose(next[event.key]);
        }}
        onPointerDown={(event) => {
          suppressClick.current = false;
          if (event.pointerType !== 'mouse' || event.button !== 0) return;
          event.currentTarget.focus({ preventScroll: true });
          drag.current = { pointerId: event.pointerId, y: event.clientY, top: event.currentTarget.scrollTop, moved: false };
        }}
        onPointerMove={(event) => {
          if (!drag.current || drag.current.pointerId !== event.pointerId) return;
          const delta = drag.current.y - event.clientY;
          if (!drag.current.moved && Math.abs(delta) < 4) return;
          if (!drag.current.moved) {
            drag.current.moved = true;
            event.currentTarget.setPointerCapture(event.pointerId);
          }
          event.preventDefault();
          event.currentTarget.scrollTop = drag.current.top + delta;
        }}
        onPointerUp={finishDrag}
        onPointerCancel={finishDrag}
        onClickCapture={(event) => {
          if (!suppressClick.current) return;
          event.preventDefault();
          event.stopPropagation();
          suppressClick.current = false;
        }}>
        {Array.from({ length: max + 1 }, (_, option) => <button type="button" role="option" id={id + '-' + option}
          aria-selected={option === value} tabIndex={-1} key={option}
          className={'date-time-wheel-option flex w-full select-none items-center justify-center rounded-md text-xs tabular-nums transition-colors '
            + (option === value ? 'font-semibold text-ide-accent' : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text')}
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => { list.current.focus({ preventScroll: true }); choose(option); }}>
          {String(option).padStart(2, '0')}
        </button>)}
      </div>
    </div>
  </div>;
}
