import React, { useState, useCallback, useMemo } from 'react';
import { Trash2, ExternalLink, MoreHorizontal, ChevronUp, Clock, Maximize2 } from 'lucide-react';
import { extractUrlsFromOcr } from '../lib/ocr_url_detector';
import { useTranslation } from 'react-i18next';

export default function PreviewActionBar({
  selectedEvent,
  selectedDetails,
  onDeleteRecord,
  onDeleteNearbyRecords,
  onOpenUrl,
  onShowMore,
  onOpenFloatingPreview,
  showOcrPanel,
}) {
  const { t } = useTranslation();
  const [showDeleteMenu, setShowDeleteMenu] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  const pageUrl = selectedDetails?.record?.page_url || null;
  const extractedUrls = useMemo(() => {
    if (pageUrl) return [pageUrl]; // Prioritize page URL if available
    return extractUrlsFromOcr(selectedDetails?.ocr_results || []).slice(0, 5); // Limit to top 5 URLs
  }, [selectedDetails?.ocr_results]);

  const hasUrls = extractedUrls.length > 0;

  const handleDeleteRecord = useCallback(async () => {
    if (!selectedEvent?.id) return;
    setIsDeleting(true);
    try {
      await onDeleteRecord?.(selectedEvent.id);
    } finally {
      setIsDeleting(false);
      setShowDeleteMenu(false);
    }
  }, [selectedEvent?.id, onDeleteRecord]);

  const handleDeleteNearby = useCallback(async () => {
    if (!selectedEvent?.timestamp) return;
    setIsDeleting(true);
    try {
      await onDeleteNearbyRecords?.(selectedEvent.timestamp, 5); // 5 minutes
    } finally {
      setIsDeleting(false);
      setShowDeleteMenu(false);
    }
  }, [selectedEvent?.timestamp, onDeleteNearbyRecords]);

  const handleOpenFirstUrl = useCallback(() => {
    if (extractedUrls.length > 0) {
      onOpenUrl?.(extractedUrls[0]);
    }
  }, [extractedUrls, onOpenUrl]);

  if (!selectedEvent) return null;

  return (
    <>
      {showDeleteMenu && (
        <div
          className="fixed inset-0 z-20"
          onClick={() => setShowDeleteMenu(false)}
        />
      )}
      <div
        className={`absolute bottom-0 left-1/2 -translate-x-1/2 z-30 flex items-end justify-center transition-all duration-300 w-[540px] ${
          showDeleteMenu
            ? 'h-24 pb-6'
            : 'h-12 pb-2 hover:h-24 hover:pb-6 group/actionbar'
        }`}
      >
        {/* Small Ball Indicator */}
        <div
          className={`absolute flex items-center justify-center w-10 h-10 preview-action-bar border border-ide-border rounded-full transition-all duration-300 transform ${
            showDeleteMenu
              ? 'bottom-6 scale-50 opacity-0 pointer-events-none'
              : 'bottom-2 scale-100 opacity-100 pointer-events-auto group-hover/actionbar:bottom-6 group-hover/actionbar:scale-50 group-hover/actionbar:opacity-0 group-hover/actionbar:pointer-events-none'
          }`}
        >
          <ChevronUp className="w-5 h-5 text-ide-muted transition-colors group-hover/actionbar:text-ide-text" />
        </div>

        {/* Full Capsule Bar */}
        <div
          className={`flex items-center gap-1 px-2 py-1.5 preview-action-bar border border-ide-border rounded-full transition-all duration-300 transform origin-center ${
            showDeleteMenu
              ? 'scale-100 opacity-100 pointer-events-auto'
              : 'scale-75 opacity-0 pointer-events-none group-hover/actionbar:scale-100 group-hover/actionbar:opacity-100 group-hover/actionbar:pointer-events-auto'
          }`}
        >
          {/* Delete Button with Split Action */}
          <div className="relative">
            <div className="flex items-center rounded-full overflow-hidden border border-ide-border">
              <button
                onClick={handleDeleteRecord}
                disabled={isDeleting}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs transition-colors hover:bg-red-500/15 text-ide-text disabled:opacity-50"
                title={t('previewAction.deleteNowTitle')}
              >
                <Trash2 className="w-3.5 h-3.5" />
                <span>{t('previewAction.delete')}</span>
              </button>
              <div className="w-px h-6 bg-ide-border/70" />
              <button
                onClick={() => setShowDeleteMenu(!showDeleteMenu)}
                disabled={isDeleting}
                className={`flex items-center px-2 py-1.5 text-xs transition-colors ${showDeleteMenu
                    ? 'bg-red-500/20 text-red-400'
                    : 'hover:bg-ide-hover text-ide-text'
                  } disabled:opacity-50`}
                title={t('previewAction.moreDeleteOptions')}
              >
                <ChevronUp className={`w-3 h-3 transition-transform ${showDeleteMenu ? 'rotate-180' : ''}`} />
              </button>
            </div>

            {showDeleteMenu && (
              <div className="absolute bottom-full left-0 mb-2 w-48 bg-ide-panel border border-ide-border rounded-lg shadow-xl overflow-hidden">
                <button
                  onClick={handleDeleteRecord}
                  disabled={isDeleting}
                  className="w-full flex items-center gap-2 px-3 py-2 text-xs text-ide-text hover:bg-ide-hover transition-colors disabled:opacity-50"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  {t('previewAction.deleteThisRecord')}
                </button>
                <div className="border-t border-ide-border" />
                <button
                  onClick={handleDeleteNearby}
                  disabled={isDeleting}
                  className="w-full flex items-center gap-2 px-3 py-2 text-xs text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50"
                >
                  <Clock className="w-3.5 h-3.5" />
                  {t('previewAction.deleteNearby5min')}
                </button>
              </div>
            )}
          </div>

          <div className="w-px h-5 bg-ide-border/50" />

          {/* Callback Button (Beta) */}
          <button
            onClick={handleOpenFirstUrl}
            disabled={!hasUrls}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs transition-colors ${hasUrls
                ? 'hover:bg-ide-hover text-ide-text'
                : 'text-ide-muted cursor-not-allowed opacity-50'
              }`}
            title={hasUrls ? t('previewAction.openUrlTitle', { url: extractedUrls[0] }) : t('previewAction.noUrlDetected')}
          >
            <ExternalLink className="w-3.5 h-3.5" />
            <span>Callback</span>
            <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">beta</span>
          </button>

          <div className="w-px h-5 bg-ide-border/50" />

          <button
            onClick={onOpenFloatingPreview}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs transition-colors hover:bg-ide-hover text-ide-text"
            title={t('previewAction.openFloatingPreview', '在独立窗口中查看')}
          >
            <Maximize2 className="w-3.5 h-3.5" />
            <span>{t('previewAction.float', '独立预览')}</span>
          </button>

          <div className="w-px h-5 bg-ide-border/50" />

          {/* More Button */}
          <button
            onClick={onShowMore}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs transition-colors ${showOcrPanel
                ? 'bg-ide-accent/20 text-ide-accent'
                : 'hover:bg-ide-hover text-ide-text'
              }`}
            title={t('previewAction.showOcr')}
          >
            <MoreHorizontal className="w-3.5 h-3.5" />
            <span>{t('previewAction.more')}</span>
          </button>
        </div>
      </div>
    </>
  );
}
