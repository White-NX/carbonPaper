import React from 'react';
import { cn } from '../../lib/utils';

/**
 * Label, counts and a bar. Pass `current`/`total` for a counted run, `percent`
 * when only a ratio is known, or neither for an indeterminate bar.
 */
export function ProgressBlock({ label, current, total, percent, showCount = true, detail, className }) {
  const hasCount = Number.isFinite(current) && Number.isFinite(total) && total > 0;
  let value = Number.isFinite(percent) ? percent : null;
  if (value === null && hasCount) value = Math.round((current / total) * 100);
  if (value !== null) value = Math.min(100, Math.max(0, value));

  return (
    <div className={cn('w-full space-y-1.5', className)}>
      {(label || (showCount && value !== null)) && (
        <div className="flex justify-between gap-3 text-xs">
          <span className="font-medium text-ide-text">{label}</span>
          {showCount && value !== null && (
            <span className="shrink-0 tabular-nums text-ide-muted">
              {hasCount ? `${current.toLocaleString()} / ${total.toLocaleString()} (${value}%)` : `${value}%`}
            </span>
          )}
        </div>
      )}
      <div
        role="progressbar"
        aria-label={typeof label === 'string' ? label : undefined}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={value ?? undefined}
        className="h-2 w-full overflow-hidden rounded-full border border-ide-border bg-ide-bg"
      >
        <div
          className={cn('h-full rounded-full bg-ide-accent transition-all duration-300 ease-out', value === null && 'w-1/3 animate-pulse')}
          style={value !== null ? { width: `${value}%` } : undefined}
        />
      </div>
      {detail && <p className="text-xs text-ide-muted">{detail}</p>}
    </div>
  );
}
