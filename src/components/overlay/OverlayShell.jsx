import React, { useId } from 'react';
import { useDialogFocus } from '../../hooks/useDialogFocus';
import { cn } from '../../lib/utils';
import { OVERLAY_LAYERS } from './layers';

const SIZES = {
  sm: 'max-w-md',
  md: 'max-w-lg',
  lg: 'max-w-xl',
  xl: 'max-w-2xl',
};

const ICON_TONES = {
  accent: 'text-ide-accent',
  warning: 'text-ide-warning',
  danger: 'text-ide-error',
};

/** Icon tile, title and subtitle. Rendered by OverlayShell when given a title. */
export function OverlayHeader({ id, icon: Icon, tone = 'accent', title, subtitle, aside, centered = false }) {
  return (
    <div className={cn('flex gap-3', centered ? 'flex-col items-center text-center' : 'items-center')}>
      {Icon && (
        <div className={cn('flex shrink-0 items-center justify-center rounded-lg border border-ide-border bg-ide-bg',
          centered ? 'h-14 w-14 rounded-xl' : 'h-10 w-10')}>
          <Icon className={cn(centered ? 'h-7 w-7' : 'h-5 w-5', ICON_TONES[tone] ?? ICON_TONES.accent)} aria-hidden="true" />
        </div>
      )}
      <div className="min-w-0 flex-1">
        <h2 id={id} className="text-lg font-semibold text-ide-text">{title}</h2>
        {subtitle && <p className={cn('text-xs text-ide-muted', centered && 'mt-1')}>{subtitle}</p>}
      </div>
      {aside && <div className="shrink-0">{aside}</div>}
    </div>
  );
}

/**
 * The frame every startup overlay is drawn in: backdrop, card, header and a
 * footer row with a quiet slot on the left and the real choices on the right.
 *
 * It covers its positioned parent, not the window, so it is meant to be
 * rendered inside the main content area. `layer` picks the stacking band from
 * `OVERLAY_LAYERS`. Without `onDismiss` the overlay cannot be closed with
 * Escape; that is how blocking overlays are expressed.
 */
export function OverlayShell({
  open = true,
  layer = 'prompt',
  size = 'md',
  onDismiss,
  icon,
  tone = 'accent',
  title,
  subtitle,
  headerAside,
  centeredHeader = false,
  footer,
  footerStart,
  ariaLabel,
  className,
  bodyClassName,
  children,
}) {
  const titleId = useId();
  const dialogRef = useDialogFocus(open, onDismiss, !onDismiss);
  if (!open) return null;

  return (
    <div className={cn('overlay-fade-in absolute inset-0 flex items-center justify-center bg-ide-bg/80 p-4 text-ide-muted backdrop-blur-sm', OVERLAY_LAYERS[layer])}>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        aria-label={title ? undefined : ariaLabel}
        tabIndex={-1}
        className={cn('overlay-pop-in flex max-h-full w-full flex-col rounded-xl border border-ide-border bg-ide-panel p-6 shadow-2xl focus:outline-none',
          SIZES[size] ?? SIZES.md, className)}
      >
        {title && (
          <div className="mb-4 shrink-0">
            <OverlayHeader id={titleId} icon={icon} tone={tone} title={title} subtitle={subtitle} aside={headerAside} centered={centeredHeader} />
          </div>
        )}
        <div className={cn('min-h-0 flex-1 space-y-3 overflow-y-auto', bodyClassName)}>
          {children}
        </div>
        {(footer || footerStart) && (
          <div className="mt-5 flex shrink-0 items-center justify-between gap-2">
            <div className="min-w-0">{footerStart}</div>
            <div className="flex items-center gap-2">{footer}</div>
          </div>
        )}
      </div>
    </div>
  );
}
