import React, { useEffect } from 'react';
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
  maxWidth = 'max-w-lg'
}) {
  useEffect(() => {
    const handleEscape = (e) => {
      if (e.key === 'Escape') onClose();
    };

    if (isOpen) {
      document.addEventListener('keydown', handleEscape);
      document.body.style.overflow = 'hidden';
    }

    return () => {
      document.removeEventListener('keydown', handleEscape);
      document.body.style.overflow = 'unset';
    };
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  return (
    <div 
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-in fade-in duration-200"
      onClick={onClose}
    >
      <div 
        className={cn(
          "relative w-full bg-ide-bg border border-ide-border rounded-lg shadow-2xl flex flex-col max-h-[90vh] animate-in zoom-in-95 duration-200",
          maxWidth,
          className
        )}
        onClick={e => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-ide-border bg-ide-panel shrink-0 rounded-t-lg">
          <h3 className="text-sm font-semibold uppercase tracking-wide text-ide-muted select-none">
            {title}
          </h3>
          <button 
            onClick={onClose}
            className="text-ide-muted hover:text-ide-text transition-colors p-1 hover:bg-ide-hover rounded"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <div className={cn("overflow-y-auto", contentClassName)}>
          {children}
        </div>
      </div>
    </div>
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
};
