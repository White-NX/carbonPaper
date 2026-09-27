import React from 'react';
import { AlertCircle, AlertTriangle, CheckCircle2, Info } from 'lucide-react';
import { cn } from '../../lib/utils';

const TONES = {
  info: { className: 'border-ide-accent/20 bg-ide-accent/5 text-ide-muted', icon: Info, iconClassName: 'text-ide-accent' },
  success: { className: 'border-emerald-500/30 bg-emerald-500/10 text-ide-info-success', icon: CheckCircle2 },
  warning: { className: 'border-ide-warning-border bg-ide-warning-bg text-ide-warning', icon: AlertTriangle },
  error: { className: 'border-red-500/30 bg-red-500/10 text-ide-error', icon: AlertCircle },
};

/**
 * Inline message box. Errors are announced (`role="alert"`); the other tones
 * are ordinary content.
 */
export function Banner({ tone = 'info', title, children, icon, action, className = '' }) {
  const config = TONES[tone] ?? TONES.info;
  const Icon = icon === false ? null : (icon ?? config.icon);
  return (
    <div role={tone === 'error' ? 'alert' : undefined}
      className={cn('flex items-start gap-2.5 rounded-lg border px-3 py-2.5 text-xs leading-relaxed', config.className, className)}>
      {Icon && <Icon className={cn('mt-0.5 h-4 w-4 shrink-0', config.iconClassName)} aria-hidden="true" />}
      <div className="min-w-0 flex-1 break-words">
        {title && <p className="mb-0.5 font-medium">{title}</p>}
        {children}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}
