import React, { useMemo, useState, useEffect } from 'react';
import { Monitor, Clock, Globe, ExternalLink } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { computeLinkScores } from '../lib/monitor_api';
import { openUrl } from '@tauri-apps/plugin-opener';

export default function LeftSidebar({ selectedEvent, selectedDetails }) {
  const { t } = useTranslation();
  const [scoredLinks, setScoredLinks] = useState([]);

  const iconSrc = useMemo(() => {
    const raw = selectedDetails?.record?.process_icon || selectedEvent?.processIcon || selectedDetails?.record?.page_icon;
    if (!raw) return null;
    // page_icon (favicon) may already be a full URL or data URI
    if (raw.startsWith('data:') || raw.startsWith('http://') || raw.startsWith('https://')) return raw;
    return `data:image/png;base64,${raw}`;
  }, [selectedDetails?.record?.process_icon, selectedEvent?.processIcon, selectedDetails?.record?.page_icon]);

  // Score visible_links when available
  useEffect(() => {
    const links = selectedDetails?.record?.visible_links;
    if (!links || links.length === 0) {
      setScoredLinks([]);
      return;
    }
    let cancelled = false;
    computeLinkScores(links)
      .then((results) => {
        if (!cancelled) setScoredLinks(results || []);
      })
      .catch((err) => {
        console.error('Failed to compute link scores:', err);
        if (!cancelled) setScoredLinks([]);
      });
    return () => { cancelled = true; };
  }, [selectedDetails?.record?.visible_links]);

  const pageUrl = selectedDetails?.record?.page_url;

  const getHostname = (url) => {
    try {
      return new URL(url).hostname;
    } catch {
      return url;
    }
  };

  const handleOpenUrl = (url) => {
    openUrl(url).catch((err) => console.error('Failed to open URL:', err));
  };

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
            {(pageUrl || scoredLinks.length > 0) && (
              <div>
                <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.links.title')}</label>
                <div className="mt-1 space-y-1">
                  {pageUrl && (
                    <div
                      className="hover:bg-ide-hover cursor-pointer rounded p-2 text-xs flex items-start gap-2 group"
                      onClick={() => handleOpenUrl(pageUrl)}
                      title={pageUrl}
                    >
                      <Globe size={14} className="shrink-0 mt-0.5 text-blue-400" />
                      <div className="overflow-hidden flex-1 min-w-0">
                        <div className="font-medium text-blue-400 truncate">{t('sidebar.links.currentPage')}</div>
                        <div className="truncate opacity-60">{getHostname(pageUrl)}</div>
                      </div>
                      <ExternalLink size={12} className="shrink-0 mt-0.5 opacity-0 group-hover:opacity-60" />
                    </div>
                  )}
                  {scoredLinks.map((link, idx) => (
                    <div
                      key={idx}
                      className="hover:bg-ide-hover cursor-pointer rounded p-2 text-xs flex items-start gap-2 group"
                      onClick={() => handleOpenUrl(link.url)}
                      title={link.text}
                    >
                      <ExternalLink size={14} className="shrink-0 mt-0.5 opacity-60" />
                      <div className="overflow-hidden flex-1 min-w-0">
                        <div className="truncate">{link.text || link.url}</div>
                        <div className="truncate opacity-60">{getHostname(link.url)}</div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </>
        ) : (
          <p>{t('sidebar.empty')}</p>
        )}
      </div>
    </aside>
  );
}
