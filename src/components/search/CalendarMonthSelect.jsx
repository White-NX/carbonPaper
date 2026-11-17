import React, { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { Check, ChevronDown } from 'lucide-react';
import { cn } from '../../lib/utils';
import { Button, focusStyle } from '../ui/Button';
import { MenuItem, MenuPanel } from '../ui/Menu';

/** The themed preset menu used by the preview zoom selector. */
export function CalendarMonthSelect({ value, onChange, options, label, boundaryRef }) {
  const [open, setOpen] = useState(false);
  const selector = useRef(null);
  const list = useRef(null);
  const menuId = useId();

  useEffect(() => {
    if (!open) return undefined;
    const outside = (event) => {
      if (!selector.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return undefined;
    const trigger = selector.current?.querySelector('button');
    const resize = () => {
      if (!list.current || !trigger) return;
      const boundary = boundaryRef?.current?.getBoundingClientRect().bottom ?? window.innerHeight;
      const available = boundary - trigger.getBoundingClientRect().bottom - 16;
      list.current.style.maxHeight = `${Math.max(32, Math.min(240, available))}px`;
    };
    resize();
    const option = list.current?.querySelector('[aria-checked="true"]:not(:disabled)')
      || list.current?.querySelector('button:not(:disabled)');
    option?.focus({ preventScroll: true });
    if (option) list.current.scrollTop = Math.max(0, option.offsetTop - list.current.clientHeight / 2 + option.offsetHeight / 2);
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(resize);
    if (boundaryRef?.current) observer?.observe(boundaryRef.current);
    window.addEventListener('resize', resize);
    return () => {
      observer?.disconnect();
      window.removeEventListener('resize', resize);
    };
  }, [open, boundaryRef]);

  const close = () => {
    selector.current?.querySelector('button')?.focus({ preventScroll: true });
    setOpen(false);
  };
  const keyDown = (event) => {
    if (event.key === 'Escape' && open) {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      if (!open) {
        setOpen(true);
        return;
      }
      const choices = [...list.current.querySelectorAll('button:not(:disabled)')];
      const index = choices.indexOf(document.activeElement);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? choices.length - 1
        : (index + (event.key === 'ArrowDown' ? 1 : -1) + choices.length) % choices.length;
      choices[next]?.focus({ preventScroll: true });
      const option = choices[next];
      if (option) {
        const bottom = option.offsetTop + option.offsetHeight;
        if (option.offsetTop < list.current.scrollTop) list.current.scrollTop = option.offsetTop;
        else if (bottom > list.current.scrollTop + list.current.clientHeight) list.current.scrollTop = bottom - list.current.clientHeight;
      }
    }
  };

  return <div ref={selector} className="relative min-w-0 flex-1" onKeyDown={keyDown}
    onBlur={(event) => {
      if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget)) setOpen(false);
    }}>
    <Button size="xs" onClick={() => setOpen((previous) => !previous)}
      className={cn('h-8 w-full justify-between gap-2 rounded-md bg-ide-bg px-2 text-xs', open && 'border-ide-accent bg-ide-hover')}
      aria-label={label} aria-haspopup="menu" aria-expanded={open} aria-controls={open ? menuId : undefined}>
      <span className="truncate">{options.find((option) => option.value === value)?.label}</span>
      <ChevronDown className="h-3.5 w-3.5 shrink-0 text-ide-muted" aria-hidden="true" />
    </Button>
    {open && <MenuPanel id={menuId} role="menu" aria-label={label}
      className="right-0 mt-1.5 min-w-[144px] overflow-hidden rounded-xl p-1.5">
      <div ref={list} className="date-time-month-options relative overflow-x-hidden overflow-y-auto overscroll-contain rounded-md">
        {options.map((option) => <MenuItem key={option.value} role="menuitemradio" aria-checked={value === option.value}
          tabIndex={-1} disabled={option.disabled}
          className={cn('justify-between rounded-lg focus-visible:bg-ide-hover focus-visible:text-ide-text', focusStyle,
            value === option.value && 'bg-ide-accent/10 text-ide-accent hover:bg-ide-accent/15 hover:text-ide-accent')}
          onClick={() => { onChange(option.value); close(); }}>
          <span>{option.label}</span>
          <Check className={cn('h-3 w-3 shrink-0', value !== option.value && 'invisible')} aria-hidden="true" />
        </MenuItem>)}
      </div>
    </MenuPanel>}
  </div>;
}
