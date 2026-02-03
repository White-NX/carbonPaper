import React, { useState } from 'react';
import Tabs from './Tabs';
import { AdvancedSearch } from './AdvancedSearch';
import { InspectorImage } from './Gallery';
import { Loader2, Copy, X } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import PreviewActionBar from './PreviewActionBar';

// OCR Content Panel Component
function OcrContentPanel({ selectedDetails, onClose, onCopyText }) {
  const ocrText = selectedDetails?.ocr_results?.map(r => r.text).join('\n') || '';
  
  return (
    <div className="absolute right-0 top-0 bottom-0 w-64 bg-ide-panel border-l border-ide-border flex flex-col z-20 shadow-xl">
      <div className="flex items-center justify-between px-3 py-2 border-b border-ide-border bg-ide-bg shrink-0">
        <span className="text-xs font-medium">OCR Content</span>
        <button
          onClick={onClose}
          className="p-1 hover:bg-ide-hover rounded transition-colors"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
      <div className="flex-1 overflow-hidden">
        <textarea
          className="w-full h-full bg-ide-bg p-3 text-xs font-mono text-ide-text resize-none focus:outline-none leading-relaxed"
          readOnly
          value={ocrText}
          placeholder={selectedDetails ? "No text detected" : "Select an image to view OCR content"}
        />
      </div>
      {selectedDetails?.ocr_results?.length > 0 && (
        <div className="p-2 border-t border-ide-border bg-ide-panel shrink-0 flex justify-end">
          <button
            onClick={() => onCopyText(ocrText)}
            className="flex items-center gap-2 px-3 py-1.5 bg-ide-bg hover:bg-ide-hover border border-ide-border rounded text-xs transition-colors"
          >
            <Copy size={12} /> Copy All
          </button>
        </div>
      )}
    </div>
  );
}

export default function MainArea({
  activeTab,
  setActiveTab,
  selectedImageSrc,
  isLoadingDetails,
  selectedEvent,
  selectedDetails,
  lastError,
  ocrBoxes,
  onAdvancedSelect,
  advancedSearchParams,
  onInspectorBoxClick,
  searchMode,
  onSearchModeChange,
  onDeleteRecord,
  onDeleteNearbyRecords,
  onCopyText,
}) {
  const [showOcrPanel, setShowOcrPanel] = useState(false);

  const handleShowMore = () => {
    setShowOcrPanel(!showOcrPanel);
  };

  const handleOpenUrl = async (url) => {
    if (!url) return;
    try {
      await openUrl(url);
    } catch (error) {
      console.error('Failed to open url', error);
    }
  };

  const handleCopyText = (text) => {
    navigator.clipboard.writeText(text);
    onCopyText?.(text);
  };

  return (
    <section className="flex flex-col bg-ide-bg overflow-hidden relative flex-1">
      <Tabs activeTab={activeTab} setActiveTab={setActiveTab} />

      <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
        <div className={`${activeTab === 'preview' ? 'flex' : 'hidden'} flex-1 items-center justify-center bg-grid-pattern bg-[length:20px_20px] p-4 overflow-hidden relative min-w-0 min-h-0`}>
          {selectedImageSrc ? (
            <div className="shadow-xl max-w-full max-h-full">
              <InspectorImage
                item={{ imageUrl: selectedImageSrc }}
                overlayBoxes={ocrBoxes}
                onBoxClick={onInspectorBoxClick}
                maxHeight="calc(100vh - 220px)"
              />
            </div>
          ) : (
            <div className="text-ide-muted text-sm flex flex-col items-center gap-2">
              {isLoadingDetails ? (
                <>
                  <Loader2 className="w-6 h-6 animate-spin" />
                  <span>Loading...</span>
                </>
              ) : (
                <div className="flex flex-col items-center gap-1 text-center">
                  <span>{selectedEvent ? (lastError || "Image not found on disk") : "No image selected"}</span>
                  {selectedEvent && <span className="text-xs opacity-50 font-mono">ID: {selectedEvent.id}</span>}
                </div>
              )}
            </div>
          )}

          {/* Preview Action Bar */}
          {activeTab === 'preview' && selectedEvent && (
            <PreviewActionBar
              selectedEvent={selectedEvent}
              selectedDetails={selectedDetails}
              onDeleteRecord={onDeleteRecord}
              onDeleteNearbyRecords={onDeleteNearbyRecords}
              onOpenUrl={handleOpenUrl}
              onShowMore={handleShowMore}
              showOcrPanel={showOcrPanel}
            />
          )}

          {/* OCR Content Panel */}
          {showOcrPanel && (
            <OcrContentPanel
              selectedDetails={selectedDetails}
              onClose={() => setShowOcrPanel(false)}
              onCopyText={handleCopyText}
            />
          )}
        </div>

        <div className={`${activeTab === 'advanced-search' ? 'flex flex-col' : 'hidden'} flex-1 w-full min-w-0 min-h-0 overflow-hidden`}>
          <AdvancedSearch
            active={activeTab === 'advanced-search'}
            searchParams={advancedSearchParams}
            onSelectResult={onAdvancedSelect}
            searchMode={searchMode}
            onSearchModeChange={onSearchModeChange}
          />
        </div>
      </div>
    </section>
  );
}
