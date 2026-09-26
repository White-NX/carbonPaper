import React, { useId, useRef } from 'react';
import { Check } from 'lucide-react';
import { cn } from '../../lib/utils';
import { focusRing } from '../ui/Button';
import { SettingsControlLabelContext, SettingsSwitch } from '../settings/SettingsControls';

function Badge({ children }) {
  return (
    <span className="rounded-full bg-ide-accent/15 px-2 py-0.5 text-[10px] font-medium text-ide-accent">
      {children}
    </span>
  );
}

function CardBody({ icon: Icon, label, description, badge, note, selected }) {
  return (
    <>
      <div className="flex items-start justify-between gap-2">
        {Icon && (
          <span className={cn('flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border transition-colors',
            selected ? 'border-ide-accent/40 bg-ide-accent/15 text-ide-accent' : 'border-ide-border bg-ide-panel text-ide-muted')}>
            <Icon className="h-4 w-4" aria-hidden="true" />
          </span>
        )}
        <span className="flex items-center gap-1.5">
          {badge && <Badge>{badge}</Badge>}
          <span className={cn('flex h-4 w-4 items-center justify-center rounded-full border transition-colors',
            selected ? 'border-ide-accent bg-ide-accent text-white' : 'border-ide-border')}>
            {selected && <Check className="h-3 w-3" aria-hidden="true" />}
          </span>
        </span>
      </div>
      <span className="mt-2 block text-sm font-medium text-ide-text">{label}</span>
      {description && <span className="mt-1 block text-xs leading-relaxed text-ide-muted">{description}</span>}
      {note && <span className="mt-2 block text-[11px] font-medium text-ide-accent">{note}</span>}
    </>
  );
}

const cardClass = (selected) => cn(
  'flex h-full flex-col rounded-xl border p-3 text-left transition-colors disabled:cursor-not-allowed disabled:opacity-50',
  focusRing,
  selected ? 'border-ide-accent bg-ide-accent/5 shadow-sm' : 'border-ide-border bg-ide-bg hover:border-ide-accent/40 hover:bg-ide-hover',
);

/**
 * One choice out of several, drawn as cards. Arrow keys move the selection,
 * as in any radio group.
 */
export function OptionCardGroup({ label, value, options, onChange, columns = 3 }) {
  const ref = useRef(null);
  const selectedIndex = Math.max(0, options.findIndex((option) => option.value === value));

  const move = (event, index) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
    if (!step) return;
    event.preventDefault();
    const next = (index + step + options.length) % options.length;
    ref.current?.querySelectorAll('[role="radio"]')[next]?.focus();
    onChange(options[next].value);
  };

  return (
    <div ref={ref} role="radiogroup" aria-label={label} className="grid gap-2"
      style={{ gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` }}>
      {options.map((option, index) => {
        const selected = option.value === value;
        return (
          <button key={option.value} type="button" role="radio" aria-checked={selected}
            tabIndex={index === selectedIndex ? 0 : -1}
            onKeyDown={(event) => move(event, index)}
            onClick={() => onChange(option.value)}
            className={cardClass(selected)}>
            <CardBody {...option} selected={selected} />
          </button>
        );
      })}
    </div>
  );
}

/** A card that is on or off on its own, for picks that are not exclusive. */
export function ToggleCard({ checked, onChange, disabled, ...body }) {
  return (
    <button type="button" role="checkbox" aria-checked={checked} disabled={disabled}
      onClick={() => onChange(!checked)} className={cardClass(checked)}>
      <CardBody {...body} selected={checked} />
    </button>
  );
}

/** Label, description and a switch, for yes/no settings. */
export function SwitchRow({ icon: Icon, label, description, note, checked, onChange }) {
  const labelId = useId();
  return (
    <div className="flex items-start gap-3 py-3">
      {Icon && (
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-ide-border bg-ide-panel text-ide-muted">
          <Icon className="h-4 w-4" aria-hidden="true" />
        </span>
      )}
      <div className="min-w-0 flex-1">
        <p id={labelId} className="text-sm font-medium text-ide-text">{label}</p>
        {description && <p className="mt-0.5 text-xs leading-relaxed text-ide-muted">{description}</p>}
        {note && checked && <p className="mt-1.5 text-[11px] leading-relaxed text-ide-warning">{note}</p>}
      </div>
      <SettingsControlLabelContext.Provider value={labelId}>
        <SettingsSwitch checked={checked} onChange={onChange} className="mt-1" />
      </SettingsControlLabelContext.Provider>
    </div>
  );
}
