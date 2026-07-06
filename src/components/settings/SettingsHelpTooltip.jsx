import React from 'react';
import { HelpCircle } from 'lucide-react';

export default function SettingsHelpTooltip({ children, variant = 'section', className = '' }) {
  const isTerm = variant === 'term';

  return (
    <span
      className={`relative inline-flex items-center group focus:outline-none ${isTerm ? 'align-middle' : ''} ${className}`}
      tabIndex={0}
    >
      <HelpCircle
        className={`${isTerm ? 'ml-1 -translate-y-px' : ''} h-3.5 w-3.5 cursor-help text-ide-muted transition-colors group-hover:text-ide-text group-focus:text-ide-text`}
        aria-hidden="true"
      />
      <span className="pointer-events-none absolute left-1/2 top-full z-50 mt-1.5 w-64 -translate-x-1/2 rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-xs font-normal leading-relaxed text-ide-muted opacity-0 shadow-xl transition-opacity group-hover:opacity-100 group-focus:opacity-100">
        {children}
      </span>
    </span>
  );
}
