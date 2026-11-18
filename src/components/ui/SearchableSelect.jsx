import React, { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown, Search } from 'lucide-react';
import { cn } from '../../lib/utils';
import { Button, focusStyle } from './Button';
import { MenuItem, MenuPanel } from './Menu';

/** The preview selector's themed panel, with searchable listbox behavior. */
export default function SearchableSelect({ id, value, onChange, options, label, placeholder,
  searchLabel, emptyLabel, narrowLabel, disabled = false, className = '' }) {
  const generatedId = useId();
  const listId = `${generatedId}-list`;
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState(null);
  const trigger = useRef(null);
  const panel = useRef(null);
  const search = useRef(null);
  const selected = options.find((option) => String(option.value) === String(value));
  const orderedOptions = useMemo(() => {
    const grouped = new Map();
    for (const option of options) {
      const name = option.group || '';
      if (!grouped.has(name)) grouped.set(name, []);
      grouped.get(name).push(option);
    }
    return [...grouped.values()].flat();
  }, [options]);
  const matches = useMemo(() => {
    const term = query.trim().toLocaleLowerCase();
    return orderedOptions.filter((option) => !option.disabled && (!term
      || [option.label, option.value, option.keywords].filter(Boolean).join(' ').toLocaleLowerCase().includes(term)));
  }, [orderedOptions, query]);
  const shown = matches.slice(0, 200);
  const groups = shown.reduce((all, option, index) => {
    const name = option.group || '';
    let group = all.find((item) => item.name === name);
    if (!group) { group = { name, options: [] }; all.push(group); }
    group.options.push({ option, index });
    return all;
  }, []);

  const close = () => { setOpen(false); trigger.current?.querySelector('button')?.focus(); };
  const choose = (option) => { onChange(option.value); close(); };
  const show = () => {
    setQuery('');
    const available = orderedOptions.filter((option) => !option.disabled).slice(0, 200);
    setActive(Math.max(0, available.findIndex((option) => String(option.value) === String(value))));
    setOpen(true);
  };

  useLayoutEffect(() => {
    if (!open || disabled) return undefined;
    const place = () => {
      const rect = trigger.current?.getBoundingClientRect();
      if (!rect) return;
      const below = window.innerHeight - rect.bottom - 12;
      const above = rect.top - 12;
      const height = Math.min(340, Math.max(below, above));
      const width = Math.min(Math.max(rect.width, 260), window.innerWidth - 24);
      setPosition({ width, left: Math.max(12, Math.min(rect.left, window.innerWidth - width - 12)),
        top: below >= Math.min(340, above) ? rect.bottom + 6 : undefined,
        bottom: below >= Math.min(340, above) ? undefined : window.innerHeight - rect.top + 6,
        maxHeight: Math.max(100, height), right: 'auto' });
    };
    place();
    search.current?.focus();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, disabled]);

  useEffect(() => {
    if (!open) return undefined;
    const outside = (event) => {
      if (!trigger.current?.contains(event.target) && !panel.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);

  useEffect(() => {
    if (open) panel.current?.querySelector(`[data-option-index="${active}"]`)?.scrollIntoView?.({ block: 'nearest' });
  }, [active, open]);

  useEffect(() => {
    setActive((current) => Math.min(current, Math.max(0, shown.length - 1)));
  }, [shown.length]);

  const keys = (event) => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); }
    else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      const last = shown.length - 1;
      setActive(event.key === 'Home' ? 0 : event.key === 'End' ? Math.max(0, last)
        : shown.length ? (active + (event.key === 'ArrowDown' ? 1 : -1) + shown.length) % shown.length : 0);
    } else if (event.key === 'Enter' && shown[active]) { event.preventDefault(); choose(shown[active]); }
    else if (event.key === 'Tab') close();
  };

  return (
    <div ref={trigger} className="relative min-w-0">
      <Button id={id} role="combobox" aria-label={label} aria-haspopup="listbox"
        aria-expanded={open && !disabled} aria-controls={open ? listId : undefined}
        value={String(value ?? '')} disabled={disabled} onClick={() => open ? close() : show()}
        onKeyDown={(event) => {
          if (['ArrowDown', 'ArrowUp'].includes(event.key)) { event.preventDefault(); show(); }
        }}
        className={cn('min-h-9 w-full min-w-0 justify-between bg-ide-panel px-3 py-2 text-left text-sm font-normal', className)}>
        <span className={cn('min-w-0 truncate', !selected && !value && 'text-ide-muted')}>
          {selected?.label || (value ? String(value) : placeholder)}
        </span>
        <ChevronDown size={14} className="shrink-0 text-ide-muted" aria-hidden="true" />
      </Button>
      {open && !disabled && createPortal(
        <MenuPanel role="presentation" className="fixed top-auto z-[60] flex flex-col overflow-hidden rounded-xl p-1.5" style={position || { visibility: 'hidden' }}>
          <div ref={panel} className="flex min-h-0 flex-col" onKeyDown={keys}>
            <div className="mb-1 flex shrink-0 items-center gap-2 border-b border-ide-border px-2 pb-2 pt-1">
              <Search size={14} className="shrink-0 text-ide-muted" aria-hidden="true" />
              <input ref={search} type="search" role="searchbox" aria-label={searchLabel} placeholder={searchLabel}
                aria-controls={listId} aria-activedescendant={shown[active] ? `${listId}-${active}` : undefined}
                value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }}
                className="min-w-0 flex-1 bg-transparent py-1 text-sm text-ide-text outline-none placeholder:text-ide-muted" />
            </div>
            <div id={listId} role="listbox" aria-label={label} className="min-h-0 overflow-y-auto overscroll-contain">
              {groups.map((group) => (
                <div key={group.name} role={group.name ? 'group' : undefined} aria-label={group.name || undefined}>
                  {group.name && <p className="px-2 py-1.5 text-[10px] font-medium text-ide-muted">{group.name}</p>}
                  {group.options.map(({ option, index }) => (
                    <MenuItem key={String(option.value)} id={`${listId}-${index}`} role="option" aria-selected={String(option.value) === String(value)}
                      value={String(option.value)}
                      data-option-index={index} tabIndex={-1} onMouseDown={(event) => event.preventDefault()} onClick={() => choose(option)}
                      onMouseMove={() => setActive(index)}
                      className={cn('gap-2 rounded-lg py-2 text-ide-text', focusStyle,
                        active === index && 'bg-ide-hover', String(option.value) === String(value) && 'bg-ide-accent/10 text-ide-accent')}>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate">{option.label}</span>
                        {option.description && <span className="mt-0.5 block truncate text-[10px] text-ide-muted">{option.description}</span>}
                      </span>
                      {option.badge && <span className="shrink-0 rounded bg-ide-hover px-1.5 py-0.5 text-[10px] text-ide-muted">{option.badge}</span>}
                      <Check size={12} className={cn('shrink-0', String(option.value) !== String(value) && 'invisible')} aria-hidden="true" />
                    </MenuItem>
                  ))}
                </div>
              ))}
              {shown.length === 0 && <p className="px-2 py-4 text-center text-xs text-ide-muted">{emptyLabel}</p>}
              {matches.length > shown.length && <p className="px-2 py-2 text-xs text-ide-muted">{narrowLabel}</p>}
            </div>
          </div>
        </MenuPanel>, document.body,
      )}
    </div>
  );
}
