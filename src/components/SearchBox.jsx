import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Image as ImageIcon, Type, Loader2, X, ChevronDown, Square } from 'lucide-react';
import { fetchThumbnail } from '../lib/monitor_api';
import { useSearchBoxController } from '../hooks/useSearchBoxController';

export function SearchBox({ onSelectResult, onSubmit, mode: controlledMode, onModeChange, backendOnline, monitorPaused, handlePauseMonitor, handleResumeMonitor }) {
    const { t } = useTranslation();
    const {
        query,
        setQuery,
        mode,
        showModeMenu,
        setShowModeMenu,
        results,
        error,
        loading,
        showResults,
        setShowResults,
        debouncedQuery,
        wrapperRef,
        inputRef,
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
        showProgressBar,
        progressFillPercent,
        taskSummaryPlaceholder,
        isDownloadingModels,
        toggleMode,
        selectMode,
        handleSelect,
        handleSubmit,
        handleCancelCluster,
    } = useSearchBoxController({
        onSelectResult,
        onSubmit,
        controlledMode,
        onModeChange,
        backendOnline,
        monitorPaused,
        handlePauseMonitor,
        handleResumeMonitor,
        t,
    });

    return (
        <div
            className="relative w-[450px] z-50 pointer-events-auto"
            ref={wrapperRef}
            onClick={(e) => e.stopPropagation()}
            data-keep-selection="true"
        >
            <div className="relative flex items-center bg-ide-panel rounded-md border border-ide-border focus-within:border-ide-accent focus-within:ring-1 focus-within:ring-ide-accent transition-all shadow-sm overflow-hidden">
                {showProgressBar && (
                    <div
                        className="pointer-events-none absolute inset-y-0 left-0 bg-sky-500/35 dark:bg-sky-400/20 transition-all duration-500"
                        style={{ width: `${progressFillPercent}%` }}
                    />
                )}
                <div className="relative z-10 flex items-center border-r border-ide-border mr-2">
                <button
                    className={`p-2 text-ide-muted hover:text-ide-text transition-colors ${backendOnline === false && mode === 'ocr' ? 'opacity-50 cursor-not-allowed' : ''}`}
                    onClick={toggleMode}
                    title={backendOnline === false && mode === 'ocr' ? t('search.nl.disabled_hint') : (mode === 'ocr' ? t('search.switchToNL') : t('search.switchToOCR'))}
                >
                    {mode === 'ocr' ? <Type size={16} /> : <ImageIcon size={16} />}
                </button>
                <button
                    className="p-2 pl-0 text-ide-muted hover:text-ide-text transition-colors"
                    onClick={(e) => {
                        e.stopPropagation();
                        setShowModeMenu(!showModeMenu);
                    }}
                    title={t('search.selectMode')}
                >
                    <ChevronDown size={14} />
                </button>
            </div>

            <input
                ref={inputRef}
                type="text"
                className="relative z-10 bg-transparent border-none outline-none text-ide-text text-sm w-full h-8 px-3 placeholder-ide-muted"
                placeholder={showProgressBar ? taskSummaryPlaceholder : (mode === 'ocr' ? t('search.placeholder.ocr') : t('search.placeholder.nl'))}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onFocus={() => setShowResults(true)}
                onKeyDown={handleSubmit}
            />
            {canCancelClusterTask && (
                <button
                    onClick={handleCancelCluster}
                    className="relative z-10 p-1 mr-2 text-rose-500 hover:text-rose-600 transition-colors shrink-0"
                    title={t('search.task.stopTooltip', '停止处理智能聚类')}
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

            {showModeMenu && (
                <div className="absolute top-full left-0 mt-2 w-72 bg-ide-panel border border-ide-border rounded-md shadow-xl z-[60] p-1 flex flex-col gap-1">
                    <button
                        className={`flex items-start gap-3 p-3 rounded hover:bg-ide-hover text-left transition-colors ${mode === 'ocr' ? 'bg-ide-active border border-ide-border' : ''}`}
                        onClick={() => selectMode('ocr')}
                    >
                        <div className="mt-1 text-ide-accent"><Type size={18} /></div>
                        <div>
                            <div className="text-sm font-bold text-ide-text">{t('search.mode.ocr.title')}</div>
                            <div className="text-xs text-ide-muted leading-relaxed">{t('search.mode.ocr.description')}</div>
                        </div>
                    </button>
                    <button
                        className={`flex items-start gap-3 p-3 rounded hover:bg-ide-hover text-left transition-colors ${mode === 'nl' ? 'bg-ide-active border border-ide-border' : ''} ${backendOnline === false ? 'opacity-50 cursor-not-allowed' : ''}`}
                        onClick={() => selectMode('nl')}
                        title={backendOnline === false ? t('search.nl.disabled_hint') : ''}
                    >
                        <div className="mt-1 text-ide-success"><ImageIcon size={18} /></div>
                        <div>
                            <div className="text-sm font-bold text-ide-text">{t('search.mode.nl.title')}</div>
                            <div className="text-xs text-ide-muted leading-relaxed">
                                {t('search.mode.nl.description')}
                                {backendOnline === false && <span className="block text-xs text-red-400 mt-1">{t('search.nl.disabled_hint')}</span>}
                            </div>
                        </div>
                    </button>
                </div>
            )}

            {showResults && (
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
                                    {t('search.task.stop', '停止')}
                                </button>
                            )}
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
                    <div className="overflow-y-auto custom-scrollbar">
                        {error ? (
                            <div className="p-4 text-center text-red-500 text-sm">
                                {t('search.searchError', { message: error })}
                            </div>
                        ) : results.length > 0 ? (
                            results.map((item, index) => (
                                <SearchResultItem
                                    key={item.id || index}
                                    item={item}
                                    mode={mode}
                                    query={debouncedQuery}
                                    onClick={() => handleSelect(item)}
                                    preloadedSrc={thumbCache[mode === 'nl' ? item.metadata?.screenshot_id : item.screenshot_id] || null}
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
                </div>
            )}
        </div>
    );
}

function SearchResultItem({ item, mode, query, onClick, preloadedSrc }) {
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
            const screenshotId = mode === 'nl' ? item.metadata?.screenshot_id : item.screenshot_id;
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
    }, [imgSrc, loadFailed, item, mode]);

    // Highlighting logic for OCR
    const renderOCRText = () => {
        const text = mode === 'nl' ? item.ocr_text : item.text;
        if (!text) return <span className="text-ide-muted italic">{t('search.noTextContent')}</span>;

        if (mode === 'nl') {
            return <span className="text-ide-muted font-light">{text.length > 150 ? text.substring(0, 150) + '...' : text}</span>;
        }

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

    const processName = mode === 'nl' ? item.metadata?.process_name : item.process_name;
    const windowTitle = mode === 'nl' ? item.metadata?.window_title : item.window_title;

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
                {mode === 'nl' && (
                    <div className="text-[10px] text-ide-muted mt-auto pt-1">
                        {t('search.matchScore')}: {(item.similarity || 0).toFixed(2)}
                    </div>
                )}
            </div>
        </div>
    );
}
