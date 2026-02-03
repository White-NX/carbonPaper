import React from 'react';
import PropTypes from 'prop-types';
import { cn } from '../lib/utils';

export function ConfirmDialog({
  isOpen,
  title,
  message,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
  confirmVariant,
  loading,
}) {
  if (!isOpen) return null;

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60 backdrop-blur-sm px-4 py-6"
      onClick={(event) => {
        event.stopPropagation();
        if (onCancel) onCancel();
      }}
    >
      <div
        className="w-full max-w-xs rounded border border-ide-border bg-ide-panel p-4 shadow-2xl transform scale-100 animate-in fade-in zoom-in duration-200"
        onClick={(event) => event.stopPropagation()}
      >
        <h3 className="text-sm font-bold text-ide-text mb-2">{title}</h3>
        {message && (
          <p className="text-xs text-ide-muted mb-4 leading-relaxed">
            {message}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="px-3 py-1.5 text-xs font-medium rounded border border-ide-border text-ide-text hover:bg-ide-hover transition-colors"
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={loading}
            className={cn(
              'px-3 py-1.5 text-xs font-medium rounded text-white transition-colors shadow-sm',
              confirmVariant === 'danger'
                ? 'bg-ide-error hover:bg-ide-error/90'
                : 'bg-ide-accent hover:bg-ide-accent/90',
              loading && 'cursor-not-allowed opacity-70'
            )}
          >
            {loading ? 'Processing…' : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

ConfirmDialog.propTypes = {
  isOpen: PropTypes.bool,
  title: PropTypes.string.isRequired,
  message: PropTypes.string,
  confirmLabel: PropTypes.string,
  cancelLabel: PropTypes.string,
  onConfirm: PropTypes.func,
  onCancel: PropTypes.func,
  confirmVariant: PropTypes.oneOf(['default', 'danger']),
  loading: PropTypes.bool,
};

ConfirmDialog.defaultProps = {
  isOpen: false,
  message: '',
  confirmLabel: 'Confirm',
  cancelLabel: 'Cancel',
  onConfirm: undefined,
  onCancel: undefined,
  confirmVariant: 'default',
  loading: false,
};

export default ConfirmDialog;
