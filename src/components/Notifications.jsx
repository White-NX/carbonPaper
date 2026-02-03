import React, { useEffect } from 'react';
import { X, CheckCircle2, XCircle, Info, Trash2 } from 'lucide-react';
import { cn } from '../lib/utils';

export function NotificationToast({ notifications, onClose }) {
  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 pointer-events-none">
      {notifications.map((n) => (
        <div 
          key={n.id} 
          className={cn(
            "pointer-events-auto min-w-[300px] max-w-[400px] p-4 rounded-lg shadow-lg border flex items-start gap-3 transition-all duration-300 animate-in slide-in-from-right-full",
            "bg-ide-panel border-ide-border text-ide-text"
          )}
        >
          {n.type === 'success' && <CheckCircle2 className="w-5 h-5 text-ide-success shrink-0" />}
          {n.type === 'error' && <XCircle className="w-5 h-5 text-ide-error shrink-0" />}
          {n.type === 'info' && <Info className="w-5 h-5 text-ide-accent shrink-0" />}
          
          <div className="flex-1 overflow-hidden">
            <h4 className="font-medium text-sm">{n.title}</h4>
            <p className="text-xs text-ide-muted mt-1 break-words max-h-24 overflow-y-auto pr-1 whitespace-pre-wrap">{n.message}</p>
            {n.details && (
              <pre className="text-[11px] text-ide-muted/80 mt-2 max-h-24 overflow-y-auto pr-1 whitespace-pre-wrap break-words">
                {n.details}
              </pre>
            )}
          </div>
          
          <button onClick={() => onClose(n.id)} className="text-ide-muted hover:text-ide-text">
            <X className="w-4 h-4" />
          </button>
        </div>
      ))}
    </div>
  );
}

export function NotificationPanel({ notifications, onClear, onDismiss, isOpen, onClosePanel }) {
  if (!isOpen) return null;

  return (
    <>
        {/* Backdrop to close on click outside */}
        <div className="fixed inset-0 z-40" onClick={onClosePanel} />
        
        <div className="absolute top-12 right-4 w-80 bg-ide-panel border border-ide-border rounded-lg shadow-xl z-50 flex flex-col max-h-[80vh]">
        <div className="p-3 border-b border-ide-border flex items-center justify-between bg-ide-bg rounded-t-lg">
            <span className="font-medium text-sm">Notifications</span>
            {notifications.length > 0 && (
                <button 
                    onClick={onClear}
                    className="text-xs text-ide-muted hover:text-ide-error flex items-center gap-1"
                >
                    <Trash2 className="w-3 h-3" /> Clear all
                </button>
            )}
        </div>
        
        <div className="overflow-y-auto flex-1 p-2 space-y-2">
            {notifications.length === 0 ? (
                <div className="text-center py-8 text-ide-muted text-sm">
                    No notifications
                </div>
            ) : (
                notifications.map(n => (
                    <div key={n.id} className="p-3 rounded bg-ide-bg border border-ide-border relative group">
                        <div className="flex gap-3">
                            <div className="mt-0.5">
                                {n.type === 'success' && <CheckCircle2 className="w-4 h-4 text-ide-success" />}
                                {n.type === 'error' && <XCircle className="w-4 h-4 text-ide-error" />}
                                {n.type === 'info' && <Info className="w-4 h-4 text-ide-accent" />}
                            </div>
                            <div className="flex-1">
                                <h5 className="text-sm font-medium">{n.title}</h5>
                                <p className="text-xs text-ide-muted mt-1">{n.message}</p>
                                <span className="text-[10px] text-ide-muted mt-2 block opacity-60">
                                    {new Date(n.timestamp).toLocaleTimeString()}
                                </span>
                            </div>
                        </div>
                        <button 
                            onClick={(e) => { e.stopPropagation(); onDismiss(n.id); }}
                            className="absolute top-2 right-2 opacity-0 group-hover:opacity-100 transition-opacity text-ide-muted hover:text-ide-text"
                        >
                            <X className="w-3 h-3" />
                        </button>
                    </div>
                ))
            )}
        </div>
        </div>
    </>
  );
}
