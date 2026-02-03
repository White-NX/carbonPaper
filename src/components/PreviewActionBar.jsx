import React, { useState, useCallback, useMemo } from 'react';
import { Trash2, ExternalLink, MoreHorizontal, ChevronUp, Clock } from 'lucide-react';
import { extractUrlsFromOcr } from '../lib/ocr_url_detector';

export default function PreviewActionBar({
  selectedEvent,
  selectedDetails,
  onDeleteRecord,
  onDeleteNearbyRecords,
  onOpenUrl,
  onShowMore,
  showOcrPanel,
}) {
  const [showDeleteMenu, setShowDeleteMenu] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  const extractedUrls = useMemo(() => {
    return extractUrlsFromOcr(selectedDetails?.ocr_results);
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
    <div className="absolute bottom-6 left-1/2 -translate-x-1/2 z-30">
      <div className="flex items-center gap-1 px-2 py-1.5 bg-ide-panel/95 backdrop-blur-sm border border-ide-border rounded-full shadow-lg">
        {/* Delete Button with Split Action */}
        <div className="relative">
          <div className="flex items-center rounded-full overflow-hidden border border-ide-border">
            <button
              onClick={handleDeleteRecord}
              disabled={isDeleting}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs transition-colors hover:bg-red-500/15 text-ide-text disabled:opacity-50"
              title="立即删除此记录"
            >
              <Trash2 className="w-3.5 h-3.5" />
              <span>删除</span>
            </button>
            <div className="w-px h-6 bg-ide-border/70" />
            <button
              onClick={() => setShowDeleteMenu(!showDeleteMenu)}
              disabled={isDeleting}
              className={`flex items-center px-2 py-1.5 text-xs transition-colors ${
                showDeleteMenu
                  ? 'bg-red-500/20 text-red-400'
                  : 'hover:bg-ide-hover text-ide-text'
              } disabled:opacity-50`}
              title="更多删除选项"
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
                删除此记录
              </button>
              <div className="border-t border-ide-border" />
              <button
                onClick={handleDeleteNearby}
                disabled={isDeleting}
                className="w-full flex items-center gap-2 px-3 py-2 text-xs text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50"
              >
                <Clock className="w-3.5 h-3.5" />
                删除附近5分钟内的记录
              </button>
            </div>
          )}
        </div>

        <div className="w-px h-5 bg-ide-border/50" />

        {/* Callback Button (Alpha) */}
        <button
          onClick={handleOpenFirstUrl}
          disabled={!hasUrls}
          className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs transition-colors ${
            hasUrls 
              ? 'hover:bg-ide-hover text-ide-text' 
              : 'text-ide-muted cursor-not-allowed opacity-50'
          }`}
          title={hasUrls ? `打开 URL: ${extractedUrls[0]}` : '未检测到 URL'}
        >
          <ExternalLink className="w-3.5 h-3.5" />
          <span>Callback</span>
          <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
        </button>

        <div className="w-px h-5 bg-ide-border/50" />

        {/* More Button */}
        <button
          onClick={onShowMore}
          className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs transition-colors ${
            showOcrPanel 
              ? 'bg-ide-accent/20 text-ide-accent' 
              : 'hover:bg-ide-hover text-ide-text'
          }`}
          title="显示OCR内容"
        >
          <MoreHorizontal className="w-3.5 h-3.5" />
          <span>更多</span>
        </button>
      </div>
      
      {/* Click outside handler */}
      {showDeleteMenu && (
        <div 
          className="fixed inset-0 z-[-1]" 
          onClick={() => setShowDeleteMenu(false)}
        />
      )}
    </div>
  );
}
