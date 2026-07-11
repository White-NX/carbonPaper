import React from 'react';
import { AlertTriangle } from 'lucide-react';

function cx(...classes) {
  return classes.filter(Boolean).join(' ');
}

export function SettingsCard({
  children,
  className = '',
  padding = 'p-5',
}) {
  return (
    <div className={cx('bg-ide-panel/60 border border-ide-border rounded-2xl', padding, className)}>
      {children}
    </div>
  );
}

export function SettingsErrorBanner({ children, className = '' }) {
  return (
    <div className={cx('shrink-0 px-4 py-2 rounded-lg border border-red-500/40 text-xs text-red-200 bg-red-500/10', className)}>
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
