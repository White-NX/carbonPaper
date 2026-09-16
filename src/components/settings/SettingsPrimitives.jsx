import React, { useId } from 'react';
import { AlertTriangle, ChevronDown } from 'lucide-react';
import { SettingsControlLabelContext } from './SettingsControls';

function cx(...classes) {
  return classes.filter(Boolean).join(' ');
}

export function SettingsCard({
  children,
  className = '',
  padding = 'p-5',
}) {
  return (
    <div className={cx('bg-ide-panel border border-ide-border rounded-2xl', padding, className)}>
      {children}
    </div>
  );
}

export function SettingsErrorBanner({ children, className = '' }) {
  return (
    <div role="alert" className={cx('shrink-0 px-4 py-2 rounded-lg border border-red-500/30 text-xs text-ide-error bg-red-500/10', className)}>
      {children}
    </div>
  );
}

export function SettingsWarningBanner({ title, children, className = '' }) {
  return (
    <div className={cx('flex items-start gap-3 px-4 py-3 rounded-lg border border-ide-warning-border bg-ide-warning-bg', className)}>
      <AlertTriangle className="w-4 h-4 text-ide-warning mt-0.5 shrink-0" />
      <div className="text-xs text-yellow-600 dark:text-yellow-500">
        {title && <p className="font-medium mb-1">{title}</p>}
        {children}
      </div>
    </div>
  );
}

export function SettingsSection({ title, description, icon: Icon, children, id, actions }) {
  const titleId = useId();
  return (
    <section id={id} aria-labelledby={titleId} className="settings-section space-y-3 scroll-mt-6">
      <div className="flex items-start justify-between gap-3 px-1">
        <div className="min-w-0">
          <h2 id={titleId} className="flex items-center gap-2 text-sm font-semibold text-ide-accent">
            {Icon && <Icon className="h-4 w-4 shrink-0" aria-hidden="true" />}{title}
          </h2>
          {description && <p className="mt-1 text-xs leading-relaxed text-ide-muted">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export function SettingsGroup({ children, className = '', ...props }) {
  return <div className={cx('rounded-xl border border-ide-border bg-ide-bg p-4 text-sm text-ide-text', className)} {...props}>{children}</div>;
}

export function SettingsRow({ label, description, control, children, className = '' }) {
  const labelId = useId();
  return (
    <div className={cx('settings-row space-y-3', className)}>
      <div className="flex flex-wrap items-center justify-between gap-x-5 gap-y-3">
        <div className="min-w-0 flex-1 basis-48">
          <div id={labelId} className="font-medium text-ide-text">{label}</div>
          {description && <p className="mt-1 text-xs leading-relaxed text-ide-muted">{description}</p>}
        </div>
        {control && <SettingsControlLabelContext.Provider value={labelId}><div className="max-w-full shrink-0">{control}</div></SettingsControlLabelContext.Provider>}
      </div>
      <SettingsControlLabelContext.Provider value={labelId}>{children}</SettingsControlLabelContext.Provider>
    </div>
  );
}

export function SettingsDivider() {
  return <div className="my-4 h-px bg-ide-border/50" role="separator" />;
}

export function SettingsDisclosure({ title, children, defaultOpen = false, open, onOpenChange, className = '' }) {
  return (
    <details className={cx('settings-disclosure', className)} open={open ?? (defaultOpen || undefined)}
      onToggle={onOpenChange ? (event) => onOpenChange(event.currentTarget.open) : undefined}>
      <summary className="flex cursor-pointer list-none items-center justify-between gap-3 rounded-lg py-2 text-xs font-medium text-ide-muted hover:text-ide-text">
        {title}<ChevronDown className="settings-disclosure-chevron h-4 w-4 shrink-0 transition-transform" />
      </summary>
      <div className="space-y-3 pt-2">{children}</div>
    </details>
  );
}

export function SettingsStatus({ children, tone = 'neutral' }) {
  const color = { neutral: 'text-ide-muted', success: 'text-ide-info-success', warning: 'text-ide-warning', error: 'text-ide-error' }[tone];
  return <p role={tone === 'error' ? 'alert' : 'status'} className={cx('text-xs leading-relaxed', color)}>{children}</p>;
}
