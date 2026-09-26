import React, { createContext, useContext, useRef } from 'react';

export const SettingsControlLabelContext = createContext(undefined);
const cx = (...classes) => classes.filter(Boolean).join(' ');
const focus = 'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent focus-visible:ring-offset-2 focus-visible:ring-offset-ide-bg';

export function SettingsSwitch({ checked, onChange, disabled = false, title, className = '', ...props }) {
  const labelId = useContext(SettingsControlLabelContext);
  return (
    <button type="button" role="switch" aria-checked={Boolean(checked)} aria-labelledby={labelId}
      aria-label={!labelId ? title : undefined} title={title} disabled={disabled}
      onClick={() => onChange?.(!checked)} {...props}
      className={cx('relative h-5 w-10 shrink-0 rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50',
        checked ? 'bg-ide-accent' : 'bg-ide-border', focus, className)}>
      <span className={cx('absolute left-0 top-0.5 h-4 w-4 rounded-full bg-white transition-transform', checked ? 'translate-x-5' : 'translate-x-0.5')} />
    </button>
  );
}

export { Button as SettingsButton } from '../ui/Button';

export function SettingsSelect({ value, onChange, options, label, className = '', disabled, ...props }) {
  const labelId = useContext(SettingsControlLabelContext);
  return (
    <select value={String(value ?? '')} disabled={disabled} aria-labelledby={labelId}
      aria-label={labelId ? undefined : label} {...props}
      onChange={(event) => {
        const option = options.find((item) => String(item.value) === event.target.value);
        if (option) onChange?.(option.value);
      }}
      className={cx('min-h-9 max-w-full rounded-lg border border-ide-border bg-ide-panel px-3 py-1.5 text-sm text-ide-text disabled:cursor-not-allowed disabled:opacity-50', focus, className)}>
      {options.map((option) => <option key={String(option.value)} value={String(option.value)} disabled={option.disabled}>{option.label}</option>)}
    </select>
  );
}

export function SettingsSegmentedControl({ value, options, onChange, columns, density = 'compact', disabled = false, className = '', label }) {
  const labelId = useContext(SettingsControlLabelContext);
  const ref = useRef(null);
  const isCard = density === 'card';
  const firstAvailable = options.findIndex((option) => !option.disabled);
  const selectedIndex = options.findIndex((option) => option.value === value);
  const focusIndex = selectedIndex < 0 || options[selectedIndex]?.disabled ? firstAvailable : selectedIndex;
  const move = (event, index) => {
    const direction = ['ArrowRight', 'ArrowDown'].includes(event.key) ? 1 : ['ArrowLeft', 'ArrowUp'].includes(event.key) ? -1 : 0;
    if (!direction || disabled) return;
    event.preventDefault();
    for (let offset = 1; offset <= options.length; offset += 1) {
      const next = (index + direction * offset + options.length) % options.length;
      if (!options[next].disabled) {
        ref.current?.querySelectorAll('[role="radio"]')[next]?.focus();
        onChange?.(options[next].value);
        break;
      }
    }
  };
  return (
    <div ref={ref} role="radiogroup" aria-labelledby={labelId} aria-label={labelId ? undefined : label}
      className={cx('settings-choices grid', isCard ? 'gap-2' : 'gap-1 rounded-lg border border-ide-border bg-ide-panel p-1', className)}
      style={columns ? { '--settings-columns': columns } : undefined}>
      {options.map((option, index) => {
        const selected = option.value === value;
        return (
          <button key={String(option.value)} type="button" role="radio" aria-checked={selected}
            tabIndex={index === focusIndex ? 0 : -1}
            disabled={disabled || option.disabled} title={option.title || option.description}
            onKeyDown={(event) => move(event, index)} onClick={() => onChange?.(option.value)}
            className={cx(isCard ? 'rounded-lg border px-3 py-2 text-left' : 'min-h-9 rounded-md border border-transparent px-2 py-1 text-xs font-medium',
              'transition-colors disabled:cursor-not-allowed disabled:opacity-50', focus,
              selected ? option.selectedClassName || (isCard ? 'border-ide-accent bg-ide-accent/10 text-ide-text' : 'bg-ide-accent text-white')
                : option.idleClassName || 'text-ide-muted hover:bg-ide-hover hover:text-ide-text', option.className)}>
            <span className={cx('block', isCard && 'text-sm font-medium')}>{option.label}</span>
            {isCard && option.description && <span className="mt-1 block text-xs leading-relaxed opacity-80">{option.description}</span>}
          </button>
        );
      })}
    </div>
  );
}
