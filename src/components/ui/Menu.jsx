import React from 'react';
import { cn } from '../../lib/utils';

export function MenuPanel({ children, className, ...props }) {
  return (
    <div {...props} className={cn('absolute right-0 top-full z-30 mt-1 min-w-[130px] rounded-lg border border-ide-border bg-ide-panel p-1 shadow-xl', className)}>
      {children}
    </div>
  );
}

export function MenuItem({ icon, children, danger = false, className, ...props }) {
  return (
    <button type="button" {...props}
      className={cn('flex w-full items-center gap-2 whitespace-nowrap rounded px-2 py-1.5 text-left text-xs transition-colors disabled:pointer-events-none disabled:opacity-40',
        danger
          ? 'text-ide-muted hover:bg-red-500/10 hover:text-red-400'
          : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text', className)}>
      {icon}
      {children}
    </button>
  );
}
