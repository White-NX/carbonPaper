import React, { useMemo } from 'react';
import { Monitor, Clock } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export default function LeftSidebar({ selectedEvent, selectedDetails }) {
  const { t } = useTranslation();
  const iconSrc = useMemo(() => {
    const raw = selectedDetails?.record?.process_icon || selectedEvent?.processIcon;
    if (!raw) return null;
    return raw.startsWith('data:') ? raw : `data:image/png;base64,${raw}`;
  }, [selectedDetails?.record?.process_icon, selectedEvent?.processIcon]);
  return (
    <aside className="ide-panel overflow-y-auto hidden md:block border-r border-ide-border">
      <div className="ide-header">
        <span>{t('sidebar.details.title')}</span>
      </div>
      <div className="p-4 text-sm text-ide-muted space-y-4">
        {selectedEvent ? (
          <>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.process')}</label>
              <div className="flex items-center gap-2 mt-1">
                <div className="w-8 h-8 rounded shrink-0 flex items-center justify-center overflow-hidden">
                  {iconSrc ? (
                    <img src={iconSrc} alt={selectedDetails?.record?.process_name || selectedEvent.appName || 'app'} className="w-6 h-6 object-cover"/>
                  ) : (
                    <div className="w-7 h-7 bg-blue-500/20 text-blue-500 flex items-center justify-center">
                      <Monitor size={16} />
                    </div>
                  )}
                </div>
                <div className="overflow-hidden">
                  <div className="font-medium truncate" title={selectedDetails?.record?.process_name || selectedEvent.appName}>
                    {selectedDetails?.record?.process_name || selectedEvent.appName}
                  </div>
                </div>
              </div>
            </div>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.window')}</label>
              <div className="mt-1 text-sm break-words opacity-80 line-clamp-3 select-text" title={selectedDetails?.record?.window_title || selectedEvent.windowTitle}>
                {selectedDetails?.record?.window_title || selectedEvent.windowTitle}
              </div>
            </div>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.time')}</label>
              <div className="flex items-center gap-2 mt-1 text-sm opacity-80">
                <Clock size={14} />
                {new Date(selectedEvent.timestamp).toLocaleString()}
              </div>
            </div>
          </>
        ) : (
          <p>{t('sidebar.empty')}</p>
        )}
      </div>
    </aside>
  );
}
