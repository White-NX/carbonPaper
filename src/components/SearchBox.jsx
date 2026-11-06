import React, { useState, useEffect, useImperativeHandle } from 'react';
import { useTranslation } from 'react-i18next';
import { Image as ImageIcon, Search, Loader2, X, Square, CornerDownLeft } from 'lucide-react';
import { fetchThumbnail } from '../lib/monitor_api';
import { useSearchBoxController } from '../hooks/useSearchBoxController';
import { ProgressBar } from './overlay/ProgressBlock';

export const SearchBox = React.forwardRef(function SearchBox({ onSelectResult, onSubmit, monitorPaused, handlePauseMonitor, handleResumeMonitor, disabled = false }, ref) {
    const { t } = useTranslation();
    const {
        query,
        setQuery,
        results,
        error,
        loading,
        showResults,
        debouncedQuery,
        wrapperRef,
        inputRef,
        handleInputFocus,
        thumbCache,
        isMigrating,
        deleteQueueStatus,
        smartClusterQueueStatus,
        downloadProgress,
        hasDeleteTask,
        deleteProgress,
        hasClusterTask,
        canCancelClusterTask,
        clusterProgress,
        hasSemanticIndexTask,
        semanticIndexTaskLabel,
        hasClipIndexTask,
        clipIndexTaskLabel,
        showProgressBar,
        progressFillPercent,
        progressActive,
        taskSummaryPlaceholder,
        isDownloadingModels,
        handleSelect,
        handleSubmit,
        handleViewMore,
        handleCancelCluster,
    } = useSearchBoxController({
        onSelectResult,
        onSubmit,
        disabled,
        monitorPaused,
        handlePauseMonitor,
        handleResumeMonitor,
        t,
    });

    useImperativeHandle(ref, () => inputRef.current, [inputRef]);

    return (
        <div
            className={`relative w-[450px] z-50 ${disabled ? 'pointer-events-none opacity-60' : 'pointer-events-auto'}`}
            aria-disabled={disabled || undefined}
            ref={wrapperRef}
            onClick={(e) => e.stopPropagation()}
            data-keep-selection="true"
            data-tauri-drag-region="false"
        >
            <div className="relative flex items-center bg-ide-panel rounded-md border border-ide-border focus-within:border-ide-accent focus-within:ring-1 focus-within:ring-ide-accent transition-all shadow-sm overflow-hidden">
                {showProgressBar && (progressFillPercent !== null || progressActive) && (
                    <ProgressBar variant="background" label={taskSummaryPlaceholder}
                        percent={progressFillPercent} active={progressActive} />
                )}
                <Search size={15} className="relative z-10 ml-3 shrink-0 text-ide-muted" aria-hidden="true" />

            <input
                ref={inputRef}
                type="text"
                disabled={disabled}
                aria-label={t('search.placeholder.unified')}
                aria-keyshortcuts="Control+K"
                className="relative z-10 bg-transparent border-none outline-none text-ide-text text-sm w-full h-8 px-3 placeholder-ide-muted"
                placeholder={showProgressBar ? taskSummaryPlaceholder : t('search.placeholder.unified')}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onFocus={handleInputFocus}
                onKeyDown={handleSubmit}
            />
            {canCancelClusterTask && (
                <button
                    onClick={handleCancelCluster}
                    className="relative z-10 p-1 mr-2 text-rose-500 hover:text-rose-600 transition-colors shrink-0"
                    title={t('search.task.stopTooltip')}
                >
                    <Square size={14} fill="currentColor" />
                </button>
            )}
            {loading ? (
                <Loader2 size={16} className="relative z-10 animate-spin text-ide-muted mr-2" />
            ) : (
                query && (
                    <button onClick={() => setQuery('')} className="relative z-10 p-1 mr-2 text-ide-muted hover:text-ide-text">
                        <X size={14} />
                    </button>
                )
            )}
            </div>

            {!disabled && showResults && (
                <div className="absolute top-full mt-2 w-[450px] bg-ide-panel border border-ide-border rounded-lg shadow-2xl overflow-hidden max-h-[600px] flex flex-col">
                    {hasDeleteTask && (
                        <div className="p-3 bg-sky-500/10 border-b border-ide-border flex flex-col gap-1 shrink-0">
                            <div className="flex items-center gap-2 text-sky-500 text-sm font-bold">
                                <Loader2 size={14} className="animate-spin" />
                                {t('search.task.runningTitle')}
                            </div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {t('search.task.runningDesc', {
                                    progress: Math.round(deleteProgress),
                                    ocr: deleteQueueStatus.pending_ocr || 0,
                                    screenshots: deleteQueueStatus.pending_screenshots || 0,
                                })}
                            </div>
                        </div>
                    )}
                    {isDownloadingModels && (
                        <div className="p-3 bg-sky-500/10 border-b border-ide-border flex flex-col gap-1 shrink-0">
                            <div className="flex items-center gap-2 text-sky-500 text-sm font-bold">
                                <Loader2 size={14} className="animate-spin" />
                                {t('search.task.modelDownloadRunningTitle')}
                            </div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {t('search.task.modelDownloadRunningDesc', {
                                    progress: Math.round(downloadProgress),
                                })}
                            </div>
                        </div>
                    )}
                    {hasClusterTask && (
                        <div className="p-3 bg-sky-500/10 border-b border-ide-border flex items-center justify-between gap-3 shrink-0">
                            <div className="flex-1 min-w-0">
                                <div className="flex items-center gap-2 text-sky-500 text-sm font-bold">
                                    <Loader2 size={14} className="animate-spin" />
                                    {t('search.task.smartClusterRunningTitle')}
                                </div>
                                <div className="text-xs text-ide-muted leading-relaxed mt-1">
                                    {t('search.task.smartClusterRunningDesc', {
                                        progress: Math.round(clusterProgress),
                                        pending: smartClusterQueueStatus.pending_count,
                                    })}
                                </div>
                            </div>
                            {canCancelClusterTask && (
                                <button
                                    onClick={handleCancelCluster}
                                    className="px-2.5 py-1 bg-rose-500 hover:bg-rose-600 text-white rounded text-xs transition-colors font-medium shrink-0"
                                >
                                    {t('search.task.stop')}
                                </button>
                            )}
                        </div>
                    )}
                    {hasSemanticIndexTask && (
                        <div className="p-3 bg-amber-500/10 border-b border-ide-border flex flex-col gap-1 shrink-0">
                            <div className="flex items-center gap-2 text-amber-500 text-sm font-bold">
                                <Loader2 size={14} className="animate-spin" />
                                {t('search.task.semanticIndexRunningTitle')}
                            </div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {semanticIndexTaskLabel}
                            </div>
                        </div>
                    )}
                    {hasClipIndexTask && (
                        <div className="p-3 bg-orange-500/10 border-b border-ide-border flex flex-col gap-1 shrink-0">
                            <div className="flex items-center gap-2 text-orange-500 text-sm font-bold">
                                <Loader2 size={14} className="animate-spin" />
                                {t('search.task.clipIndexRunningTitle')}
                            </div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {clipIndexTaskLabel}
                            </div>
                        </div>
                    )}
                    {isMigrating && (
                        <div className="p-3 bg-yellow-500/10 border-b border-ide-border flex flex-col gap-1 shrink-0">
                            <div className="flex items-center gap-2 text-yellow-500 text-sm font-bold">
                                <Loader2 size={14} className="animate-spin" />
                                {t('settings.storageManagement.migration.search_unavailable_title')}
                            </div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {t('settings.storageManagement.migration.search_unavailable_desc')}
                            </div>
                        </div>
                    )}
                    <div className="overflow-y-auto custom-scrollbar min-h-0 flex-1">
                        {error ? (
                            <div className="p-4 text-center text-red-500 text-sm">
                                {t('search.searchError', { message: error })}
                            </div>
                        ) : results.length > 0 ? (
                            results.map((item, index) => (
                                <SearchResultItem
                                    key={item.id || index}
                                    item={item}
                                    query={debouncedQuery}
                                    onClick={() => handleSelect(item)}
                                    preloadedSrc={thumbCache[item.screenshot_id] || null}
                                />
                            ))
                            ) : loading ? (
                                <div className="p-4 text-center text-ide-muted text-sm">{t('search.loading')}</div>
                            ) : (
                                // Show "no contents" only if not currently showing the migration warning alone
                                (!isMigrating || query.trim().length > 0) && (
                                    <div className="p-4 text-center text-ide-muted text-sm">{t('search.noContents')}</div>
                                )
                        )}
                    </div>
                    {!error && (query.trim().length > 0 || results.length > 0) && (
                        <button
                            type="button"
                            onClick={handleViewMore}
                            className="shrink-0 flex items-center gap-2 px-3.5 py-2.5 bg-ide-panel hover:bg-ide-hover border-t border-ide-border text-xs text-ide-muted hover:text-ide-text transition-colors cursor-pointer text-left w-full select-none group"
                        >
                            <CornerDownLeft size={13} className="text-ide-muted group-hover:text-ide-accent transition-colors shrink-0" />
                            <span className="truncate">{t('search.pressEnterToViewMore')}</span>
                        </button>
                    )}
                </div>
            )}
        </div>
    );
});

function SearchResultItem({ item, query, onClick, preloadedSrc }) {
    const { t } = useTranslation();
    const [imgSrc, setImgSrc] = useState(preloadedSrc);
    const [loadFailed, setLoadFailed] = useState(false);

    useEffect(() => {
        if (preloadedSrc) {
            setImgSrc(preloadedSrc);
            setLoadFailed(false);
        }
    }, [preloadedSrc]);

    // Fallback: individually fetch if batch didn't provide a thumbnail
    useEffect(() => {
        if (imgSrc || loadFailed) return;
        let active = true;
        const timer = setTimeout(async () => {
            const screenshotId = item.screenshot_id;
            const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
            const path = item.image_path || item.metadata?.image_path || item.path;
            if (!id && !path) { if (active) setLoadFailed(true); return; }
            try {
                const src = await fetchThumbnail(id, id ? null : path);
                if (active) {
                    if (src) setImgSrc(src);
                    else setLoadFailed(true);
                }
            } catch {
                if (active) setLoadFailed(true);
            }
        }, 100);
        return () => { active = false; clearTimeout(timer); };
    }, [imgSrc, loadFailed, item]);

    // Highlighting logic for OCR
    const renderOCRText = () => {
        const text = item.text;
        if (!text) return <span className="text-ide-muted italic">{t('search.noTextContent')}</span>;

        // Simple highlight for OCR keywords
        if (!query) return <span className="text-ide-muted font-light">{text}</span>;

        // 按空格拆分为多个关键词，逐一转义后用 | 连接
        const tokens = query.trim().split(/\s+/).filter(Boolean);
        const escaped = tokens.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
        const pattern = new RegExp(`(${escaped.join('|')})`, 'gi');
        const lowered = tokens.map(t => t.toLowerCase());
        const parts = text.split(pattern);

        return (
            <span className="text-ide-muted font-light">
                {parts.map((part, i) =>
                    lowered.some(t => part.toLowerCase() === t)
                        ? <b key={i} className="text-ide-accent font-bold">{part}</b>
                        : part
                )}
            </span>
        );
    };

    const processName = item.process_name;
    const windowTitle = item.window_title;

    return (
        <div
            className="flex items-start gap-3 p-3 hover:bg-ide-hover cursor-pointer border-b border-ide-border transition-colors group"
            onClick={(e) => { e.stopPropagation(); onClick(); }}
        >
            <div className="w-28 h-20 flex-shrink-0 bg-ide-bg rounded-md overflow-hidden border border-ide-border shadow-sm relative group-hover:border-ide-accent transition-colors">
                {imgSrc ? (
                    <img src={imgSrc} alt="" className="w-full h-full object-cover" />
                ) : loadFailed ? (
                    <div className="w-full h-full flex items-center justify-center">
                        <ImageIcon size={16} className="text-ide-muted opacity-50" />
                    </div>
                ) : (
                    <div className="w-full h-full flex items-center justify-center">
                        <Loader2 size={16} className="animate-spin text-ide-muted" />
                    </div>
                )}
            </div>
            <div className="flex-1 min-w-0 flex flex-col gap-1 py-0.5">
                <div className="text-sm text-ide-text truncate font-bold flex items-baseline">
                    <span className="text-ide-accent mr-2">{processName || t('search.unknownProcess')}</span>
                </div>
                {windowTitle && (
                    <div className="text-xs text-ide-muted truncate mb-1">
                        {windowTitle}
                    </div>
                )}
                <div className="text-xs leading-relaxed line-clamp-2 break-all">
                    {renderOCRText()}
                </div>
            </div>
        </div>
    );
}
