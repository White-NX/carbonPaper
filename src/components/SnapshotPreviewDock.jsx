import React, { useMemo } from 'react';
import { Image as ImageIcon, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import SnapshotViewer from './SnapshotViewer';
import { Button } from './ui/Button';
import { useSnapshotPreviewData } from '../hooks/useSnapshotPreviewData';
import { getSnapshotPreviewKey } from '../lib/snapshot_preview';

/** Native windows own movement and resizing; their content shares the contextual viewer. */
export default function SnapshotPreviewDock({
  tabs = [], activeKey, onActiveChange, onCloseTab, onClear, onOpenInMainPreview, onOpenStandalone,
  isAuthenticated = true,
}) {
  const { t } = useTranslation();
  const activeTab = useMemo(() => tabs.find((tab) => getSnapshotPreviewKey(tab) === activeKey) || tabs[0], [activeKey, tabs]);
  const data = useSnapshotPreviewData(activeTab, isAuthenticated);
  const selectedKey = getSnapshotPreviewKey(activeTab);
  const moveTab = (event) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const index = tabs.findIndex((tab) => getSnapshotPreviewKey(tab) === selectedKey);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1
      : (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
    onActiveChange(getSnapshotPreviewKey(tabs[next]));
    event.currentTarget.querySelectorAll('[data-snapshot-tab]')[next]?.focus();
  };
  if (!tabs.length || !isAuthenticated) return null;
  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden bg-ide-bg">
      <div className="flex shrink-0 items-center border-b border-ide-border bg-ide-panel">
        <div role="tablist" aria-label={t('snapshotPreview.windowTitle')} onKeyDown={moveTab} className="flex min-w-0 flex-1 overflow-x-auto">
          {tabs.map((tab, index) => {
            const key = getSnapshotPreviewKey(tab), selected = key === selectedKey;
            const label = tab.window_title || tab.process_name || t('snapshotPreview.snapshotWithId', { id: tab.screenshot_id });
            return <div key={key} className={`flex max-w-64 shrink-0 items-center border-r border-ide-border ${selected ? 'bg-ide-bg' : ''}`}>
              <Button role="tab" data-snapshot-tab aria-selected={selected} tabIndex={selected ? 0 : -1}
                aria-controls={`native-preview-${index}`} id={`native-preview-tab-${index}`} variant="ghost" size="xs" icon={ImageIcon}
                onClick={() => onActiveChange(key)} title={label} className={`workspace-focus min-w-0 flex-1 justify-start rounded-none px-3 py-2 ${selected ? 'text-ide-accent' : ''}`}>
                <span className="truncate">{label}</span>
              </Button>
              <Button variant="ghost" size="xs" icon={X} title={t('snapshotPreview.closeTab')}
                aria-label={t('snapshotPreview.closeNamedTab', { title: label })} onClick={() => onCloseTab(key)} className="workspace-focus mx-1 h-6 w-6 px-0" />
            </div>;
          })}
        </div>
        <Button variant="ghost" size="xs" icon={X} onClick={onClear} title={t('snapshotPreview.closeAll')}
          aria-label={t('snapshotPreview.closeAll')} className="workspace-focus mx-2 h-7 w-7 px-0" />
      </div>
      <div role="tabpanel" id={`native-preview-${tabs.indexOf(activeTab)}`} aria-labelledby={`native-preview-tab-${tabs.indexOf(activeTab)}`} className="min-h-0 flex-1">
        <SnapshotViewer item={activeTab} imageSrc={data.imageSrc} details={data.details} loading={data.loading} error={data.error} onRetry={data.retry}
          onOpenTimeline={onOpenInMainPreview} onOpenStandalone={onOpenStandalone} isAuthenticated={isAuthenticated} />
      </div>
    </div>
  );
}
