import React from 'react';
import Tabs from './Tabs';
import { AdvancedSearch } from './AdvancedSearch';
import { InspectorImage } from './Gallery';
import { About } from './About';
import { Analysis } from './Analysis';
import { Loader2 } from 'lucide-react';

export default function MainArea({
  activeTab,
  setActiveTab,
  selectedImageSrc,
  isLoadingDetails,
  selectedEvent,
  lastError,
  ocrBoxes,
  onAdvancedSelect,
  advancedSearchParams,
  onInspectorBoxClick,
  searchMode,
  onSearchModeChange
}) {
  return (
    <section className="flex flex-col bg-ide-bg overflow-hidden relative flex-1">
      <Tabs activeTab={activeTab} setActiveTab={setActiveTab} />

      <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
        <div className={`${activeTab === 'preview' ? 'flex' : 'hidden'} flex-1 items-center justify-center bg-grid-pattern bg-[length:20px_20px] p-8 overflow-hidden`}>
          {selectedImageSrc ? (
            <div className="relative shadow-xl border border-ide-border bg-black max-w-full max-h-full flex items-center justify-center w-auto h-auto min-w-[200px] min-h-[200px]">
              <div className="relative w-full h-full">
                <InspectorImage
                  item={{ imageUrl: selectedImageSrc }}
                  overlayBoxes={ocrBoxes}
                  onBoxClick={onInspectorBoxClick}
                />
              </div>
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
        <div className={`${activeTab === 'analysis' ? 'flex flex-col' : 'hidden'} flex-1 w-full min-w-0 min-h-0 overflow-hidden`}>
          <Analysis />
        </div>
        <div className={`${activeTab === 'about' ? 'flex flex-col' : 'hidden'} flex-1 w-full min-w-0 min-h-0 overflow-hidden`}><About /></div>
      </div>
    </section>
  );
}
