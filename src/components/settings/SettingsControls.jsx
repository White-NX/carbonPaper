import React from 'react';

function cx(...classes) {
  return classes.filter(Boolean).join(' ');
}

export function SettingsSwitch({
  checked,
  onChange,
  disabled = false,
  title,
  className = '',
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={Boolean(checked)}
      disabled={disabled}
      title={title}
      onClick={() => onChange?.(!checked)}
      className={cx(
        'relative w-10 h-5 shrink-0 rounded-full transition-colors',
        disabled
          ? 'bg-ide-border opacity-50 cursor-not-allowed'
          : checked
            ? 'bg-ide-accent'
            : 'bg-ide-border',
        className,
      )}
    >
      <span
        className={cx(
          'absolute left-0 top-0.5 w-4 h-4 rounded-full bg-white transition-transform',
          checked ? 'translate-x-5' : 'translate-x-0.5',
        )}
      />
    </button>
  );
}

const BUTTON_VARIANTS = {
  primary: 'bg-ide-accent hover:bg-ide-accent/90 text-white border-transparent',
  secondary: 'bg-ide-panel border-ide-border hover:bg-ide-hover/40 text-ide-text',
  ghost: 'border-transparent text-ide-muted hover:text-ide-text hover:bg-ide-hover',
  danger: 'text-red-400 hover:text-red-300 hover:bg-red-500/10 border-red-500/30',
};

const BUTTON_SIZES = {
  xs: 'px-2 py-1 text-xs',
  sm: 'px-3 py-1.5 text-xs',
  md: 'px-4 py-2 text-sm',
};

export function SettingsButton({
  children,
  icon: Icon,
  variant = 'secondary',
  size = 'sm',
  className = '',
  disabled = false,
  ...props
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      className={cx(
        'inline-flex items-center justify-center gap-1.5 rounded border font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed',
        BUTTON_VARIANTS[variant] || BUTTON_VARIANTS.secondary,
        BUTTON_SIZES[size] || BUTTON_SIZES.sm,
        className,
      )}
      {...props}
    >
      {Icon && (React.isValidElement(Icon) ? Icon : <Icon className="w-3.5 h-3.5" />)}
      {children}
    </button>
  );
}

export function SettingsSegmentedControl({
  value,
  options,
  onChange,
  columns,
  density = 'compact',
  disabled = false,
  className = '',
}) {
  const isCard = density === 'card';

  return (
    <div
      className={cx(
        isCard
          ? 'grid gap-2'
          : 'grid gap-1 rounded-lg border border-ide-border bg-ide-panel p-1',
        className,
      )}
      style={columns ? { gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` } : undefined}
    >
      {options.map((option) => {
        const selected = option.value === value;
        const optionDisabled = disabled || option.disabled;
        const selectedClass = option.selectedClassName
          || (isCard
            ? 'border-ide-accent bg-ide-accent/15 text-ide-text'
            : 'bg-ide-accent text-white');
        const idleClass = option.idleClassName
          || (isCard
            ? 'border-ide-border bg-ide-panel/40 text-ide-muted hover:border-ide-accent/50 hover:bg-ide-hover hover:text-ide-text'
            : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text');

        return (
          <button
            key={option.value}
            type="button"
            disabled={optionDisabled}
            title={option.title || option.description}
            onClick={() => onChange?.(option.value)}
            className={cx(
              isCard
                ? 'min-h-[74px] rounded-lg border px-3 py-2 text-left transition-colors'
                : 'min-h-[34px] rounded-md px-2 py-1 text-xs font-medium transition-colors',
              selected ? selectedClass : idleClass,
              optionDisabled && !selected
                ? isCard
                  ? 'opacity-50 cursor-not-allowed hover:border-ide-border hover:bg-ide-panel/40 hover:text-ide-muted'
                  : 'opacity-60 cursor-default hover:bg-transparent hover:text-ide-muted'
                : '',
              option.className,
            )}
          >
            <span className={cx('block', isCard ? 'text-xs font-semibold' : '')}>
              {option.label}
            </span>
            {isCard && option.description && (
              <span className="mt-1 block text-[11px] leading-snug opacity-80">
                {option.description}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
