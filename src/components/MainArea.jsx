import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AdvancedSearch } from './AdvancedSearch';
import SmartClustersView from './SmartClustersView';
import RecapView from './recap/RecapView';
import EmptyPreview from './EmptyPreview';
import SnapshotViewer from './SnapshotViewer';
import SnapshotPreviewDialog from './SnapshotPreviewDialog';
import { captureDateOf } from '../lib/search_grouping';
import { getSnapshotPreviewKey, normalizeSnapshotPreviewItem, sanitizeSnapshotPreviewItem } from '../lib/snapshot_preview';
import { workspaceLabelKey } from '../lib/workspace_navigation';

export default function MainArea({
  activeTab, setActiveTab, selectedImageSrc, isLoadingDetails, selectedEvent, selectedDetails, lastError,
  onAdvancedSelect, advancedSearchParams, searchMode, onSearchModeChange, onDeleteRecord, onDeleteNearbyRecords,
  onCopyText, isAuthenticated, onTimelineSearchChange, onOpenSnapshotPreview, searchFocusRequest, previewRequest,
  onRetrySnapshot,
  recordsRevision = 0,
}) {
  const { t } = useTranslation();
  const [inlineItem, setInlineItem] = useState(null);
  const originRef = useRef(null);
  const lastPreviewRequest = useRef(null);
  const [recapNavigation, setRecapNavigation] = useState(null);

  const openInlinePreview = useCallback((item, options = {}) => {
    const normalized = normalizeSnapshotPreviewItem(item, {
      sourceLabel: item?.sourceLabel || t(workspaceLabelKey(activeTab)), sourceType: item?.sourceType || activeTab, ...options,
    });
    if (!getSnapshotPreviewKey(normalized)) return;
    originRef.current = document.activeElement;
    setInlineItem(sanitizeSnapshotPreviewItem(normalized));
  }, [activeTab, t]);

  useEffect(() => {
    if (!previewRequest || previewRequest.token === lastPreviewRequest.current) return;
    lastPreviewRequest.current = previewRequest.token;
    openInlinePreview(previewRequest.item);
  }, [openInlinePreview, previewRequest]);

  const handleOpenRecap = useCallback((navigation) => {
    setInlineItem(null);
    setRecapNavigation({ ...navigation, token: Date.now() });
    setActiveTab('recap');
  }, [setActiveTab]);

  const currentItem = useMemo(() => selectedEvent ? normalizeSnapshotPreviewItem({
    ...selectedEvent, screenshot_id: selectedEvent.id, image_path: selectedEvent.path || selectedEvent.image_path,
    process_name: selectedDetails?.record?.process_name || selectedEvent.appName,
    window_title: selectedDetails?.record?.window_title || selectedEvent.windowTitle,
    category: selectedDetails?.record?.category || selectedEvent.category,
    captured_at_ms: captureDateOf(selectedDetails?.record)?.getTime() ?? selectedEvent.timestamp ?? null,
  }, { sourceLabel: t('activityBar.records'), sourceType: 'main-preview' }) : null, [selectedDetails, selectedEvent, t]);

  return (
    <section className="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-ide-bg">
      <div className={`${activeTab === 'preview' ? 'flex' : 'hidden'} min-h-0 min-w-0 flex-1 flex-col overflow-hidden`}>
        {currentItem && isAuthenticated ? <SnapshotViewer item={currentItem} imageSrc={selectedImageSrc} details={selectedDetails}
          loading={isLoadingDetails} error={lastError} onRetry={onRetrySnapshot} active={activeTab === 'preview'} isAuthenticated={isAuthenticated}
          onOpenStandalone={onOpenSnapshotPreview} onOpenRecap={handleOpenRecap} onCopyText={onCopyText}
          onDeleteRecord={onDeleteRecord} onDeleteNearbyRecords={onDeleteNearbyRecords} /> : <EmptyPreview />}
      </div>
      <div className={`${activeTab === 'advanced-search' ? 'flex' : 'hidden'} min-h-0 min-w-0 flex-1 flex-col overflow-hidden`}>
        <AdvancedSearch active={activeTab === 'advanced-search'} searchParams={advancedSearchParams}
          onSelectResult={openInlinePreview} onOpenSnapshotPreview={onOpenSnapshotPreview}
          searchMode={searchMode} onSearchModeChange={onSearchModeChange} onTimelineSearchChange={onTimelineSearchChange}
          isAuthenticated={isAuthenticated} focusRequest={searchFocusRequest} recordsRevision={recordsRevision} onOpenRecapDay={(date) => handleOpenRecap({ date })} />
      </div>
      <div className={`${activeTab === 'smart-cluster' ? 'flex' : 'hidden'} min-h-0 min-w-0 flex-1 flex-col overflow-hidden`}>
        <SmartClustersView isAuthenticated={isAuthenticated} active={activeTab === 'smart-cluster'}
          onOpenSnapshotPreview={onOpenSnapshotPreview} onSelectScreenshot={openInlinePreview} />
      </div>
      <RecapView active={activeTab === 'recap'} isAuthenticated={isAuthenticated} onSelectScreenshot={openInlinePreview} hasWorkspaceHeader
        onOpenSnapshotPreview={onOpenSnapshotPreview} navigation={recapNavigation} />
      <SnapshotPreviewDialog item={inlineItem} onClose={() => setInlineItem(null)}
        closeLabel={t('workspace.backTo', { page: t(workspaceLabelKey(activeTab)) })}
        onOpenTimeline={(item) => { setInlineItem(null); onAdvancedSelect?.(item, { trigger: originRef.current }); }}
        onOpenStandalone={onOpenSnapshotPreview} onOpenRecap={handleOpenRecap} />
    </section>
  );
}
