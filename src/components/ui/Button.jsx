import React from 'react';
import { Loader2 } from 'lucide-react';
import { cn } from '../../lib/utils';

export const focusRing = 'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent focus-visible:ring-offset-2 focus-visible:ring-offset-ide-bg';

const VARIANTS = {
  primary: 'border-transparent bg-ide-accent text-white hover:opacity-90',
  secondary: 'border-ide-border bg-ide-panel text-ide-text hover:bg-ide-hover',
  ghost: 'border-transparent text-ide-muted hover:bg-ide-hover hover:text-ide-text',
  danger: 'border-red-500/30 text-ide-error hover:bg-red-500/10',
};
const SIZES = { xs: 'px-2 py-1 text-xs', sm: 'px-3 py-1.5 text-xs', md: 'px-4 py-2 text-sm' };

/**
 * The one button used by settings and overlays.
 *
 * `loading` swaps the icon for a spinner and disables the button, so callers do
 * not repeat the `busy ? <Loader2/> : <Icon/>` pattern.
 */
export function Button({
  children,
  icon: Icon,
  variant = 'secondary',
  size = 'sm',
  loading = false,
  className = '',
  disabled = false,
  ...props
}) {
  let iconNode = null;
  if (loading) iconNode = <Loader2 className="h-3.5 w-3.5 animate-spin" aria-hidden="true" />;
  else if (Icon) iconNode = React.isValidElement(Icon) ? Icon : <Icon className="h-3.5 w-3.5" aria-hidden="true" />;
  return (
    <button type="button" disabled={disabled || loading} {...props}
      className={cn('inline-flex items-center justify-center gap-1.5 rounded-lg border font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50',
        VARIANTS[variant], SIZES[size], focusRing, className)}>
      {iconNode}
      {children}
    </button>
  );
}
