import React, { useId } from 'react';
import { createPortal } from 'react-dom';
import { useDialogFocus, useDialogVisibility } from '../hooks/useDialogFocus';
import { useTranslation } from 'react-i18next';
import PropTypes from 'prop-types';
import { X } from 'lucide-react';
import { cn } from '../lib/utils';

export function Dialog({ 
  isOpen, 
  onClose, 
  title, 
  children, 
  className,
  contentClassName,
  maxWidth = 'max-w-lg',
  disableClose = false,
  hideCloseButton = false,
}) {
  const titleId = useId();
  const { t } = useTranslation();
  const visible = useDialogVisibility(isOpen);
  const dialogRef = useDialogFocus(visible, onClose, disableClose);

  if (!visible) return null;

  return createPortal(
    <div 
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-in fade-in duration-200"
      onClick={disableClose ? undefined : onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className={cn(
          "relative w-full bg-ide-bg border border-ide-border rounded-lg shadow-2xl flex flex-col max-h-[90vh] animate-in zoom-in-95 duration-200",
          maxWidth,
          className
        )}
        onClick={e => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-ide-border bg-ide-panel shrink-0 rounded-t-lg">
          <h3 id={titleId} className="text-sm font-semibold text-ide-muted select-none">
            {title}
          </h3>
          {!hideCloseButton && !disableClose && (
            <button 
              aria-label={t('common.close')}
              onClick={onClose}
              className="text-ide-muted hover:text-ide-text transition-colors p-1 hover:bg-ide-hover rounded"
            >
              <X className="w-5 h-5" />
            </button>
          )}
        </div>

        <div className={cn("min-h-0 flex-1 overflow-y-auto", contentClassName)}>
          {children}
        </div>
      </div>
    </div>, document.body
  );
}

Dialog.propTypes = {
  isOpen: PropTypes.bool.isRequired,
  onClose: PropTypes.func.isRequired,
  title: PropTypes.node,
  children: PropTypes.node,
  className: PropTypes.string,
  contentClassName: PropTypes.string,
  maxWidth: PropTypes.string,
  disableClose: PropTypes.bool,
  hideCloseButton: PropTypes.bool,
};
