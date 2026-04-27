import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Image as ImageIcon, Type, Loader2, X, ChevronDown } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { searchScreenshots, fetchThumbnailBatch, fetchImage, getSoftDeleteQueueStatus } from '../lib/monitor_api';

// Simple debounce hook
function useDebounce(value, delay) {
    const [debouncedValue, setDebouncedValue] = useState(value);
    useEffect(() => {
        const handler = setTimeout(() => {
            setDebouncedValue(value);
        }, delay);
        return () => {
            clearTimeout(handler);
        };
    }, [value, delay]);
    return debouncedValue;
}

export function SearchBox({ onSelectResult, onSubmit, mode: controlledMode, onModeChange, backendOnline }) {
    const { t } = useTranslation();
    const [query, setQuery] = useState('');
    const [localMode, setLocalMode] = useState('ocr'); // 'ocr' | 'nl'
    const [showModeMenu, setShowModeMenu] = useState(false);
    const [results, setResults] = useState([]);
    const [loading, setLoading] = useState(false);
    const [showResults, setShowResults] = useState(false);
    const debouncedQuery = useDebounce(query, 500);
    const wrapperRef = useRef(null);
    const inputRef = useRef(null);
    const mode = controlledMode ?? localMode;
    const setMode = onModeChange ?? setLocalMode;
    // Track if mode change came from user interaction within this component
    const userInteractionRef = useRef(false);

    // Auto-switch back to OCR mode when backend goes offline
    useEffect(() => {
        if (backendOnline === false && mode === 'nl') {
            setMode('ocr');
        }
    }, [backendOnline, mode, setMode]);

    useEffect(() => {
        function handleClickOutside(event) {
            if (wrapperRef.current && !wrapperRef.current.contains(event.target)) {
                setShowModeMenu(false);
                setShowResults(false);
            }
        }
        document.addEventListener("mousedown", handleClickOutside);
        return () => {
            document.removeEventListener("mousedown", handleClickOutside);
        };
    }, [wrapperRef]);

    useEffect(() => {
        if (debouncedQuery.trim().length === 0) {
            setResults([]);
            return;
        }

        const doSearch = async () => {
            setLoading(true);
            try {
                const res = await searchScreenshots(debouncedQuery, mode);
                setResults(res);
                // Only show results if the input is focused or user triggered the mode change
                const isFocused = document.activeElement === inputRef.current;
                if (isFocused || userInteractionRef.current) {
                    setShowResults(true);
                }
                userInteractionRef.current = false;
            } finally {
                setLoading(false);
            }
        };

        doSearch();
    }, [debouncedQuery, mode]);

    // Batch-load thumbnails when results change
    const [thumbCache, setThumbCache] = useState({});
    useEffect(() => {
        if (!results.length) { setThumbCache({}); return; }
        let active = true;
        const ids = results.map(item => {
            const sid = mode === 'nl' ? item.metadata?.screenshot_id : item.screenshot_id;
            return typeof sid === 'number' && sid > 0 ? sid : null;
        }).filter(Boolean);
        if (ids.length === 0) return;
        fetchThumbnailBatch([...new Set(ids)])
            .then(batch => { if (active && batch) setThumbCache(batch); })
            .catch(() => {});
        return () => { active = false; };
    }, [results, mode]);

    const handleSelect = (item) => {
        // Determine ID based on mode/structure
        let id = null;
        if (mode === 'ocr') {
            id = item.screenshot_id;
        } else {
            // NL search returns metadata with screenshot_id if we have it
            id = item.metadata?.screenshot_id;
            // Fallback to -1 or parse from image path hash if we really had to, but keeping it simple for now
        }

        // Normalize the path field - search results use 'image_path', but App.jsx expects 'path'
        const normalizedItem = {
            id: id,
            ...item,
            path: item.image_path || item.path, // Ensure 'path' is set
        };
        
        onSelectResult(normalizedItem);
        setShowResults(false);
    };

    const [isMigrating, setIsMigrating] = useState(false);
    const [deleteQueueStatus, setDeleteQueueStatus] = useState({
        pending_screenshots: 0,
        pending_ocr: 0,
        running: false,
    });
    const [deleteQueuePeak, setDeleteQueuePeak] = useState(0);

    const pendingDeleteTotal = Number(deleteQueueStatus?.pending_ocr || 0) + Number(deleteQueueStatus?.pending_screenshots || 0);
    const hasDeleteTask = Boolean(deleteQueueStatus?.running) || pendingDeleteTotal > 120;
    const deleteProgress = (() => {
        if (!hasDeleteTask) return 0;
        if (pendingDeleteTotal <= 0) return 100;
        if (deleteQueuePeak <= 0) return 0;
        const ratio = ((deleteQueuePeak - pendingDeleteTotal) / deleteQueuePeak) * 100;
        return Math.max(0, Math.min(100, ratio));
    })();
    const progressFillPercent = hasDeleteTask
        ? (deleteProgress <= 0 ? 8 : Math.min(100, deleteProgress))
        : 0;

    const taskSummaryPlaceholder = t('search.task.summaryPlaceholder', { progress: Math.round(deleteProgress) });

    // Active detection: check on mount and listen for progress events
    useEffect(() => {
        let active = true;
        const check = async () => {
            try {
                const status = await invoke('storage_check_hmac_migration_status');
                if (active && (status.needs_migration || status.is_running)) {
                    setIsMigrating(true);
                }
            } catch (e) { console.error(e); }
        };
        check();

        // Listen for progress events to catch an ongoing migration immediately
        let unlistenProgress = null;
        listen('hmac-migration-progress', () => {
            if (active) setIsMigrating(true);
        }).then(fn => unlistenProgress = fn);

        // Listen for completion to clear the warning
        let unlistenComplete = null;
        listen('hmac-migration-complete', () => {
            if (active) setIsMigrating(false);
        }).then(fn => unlistenComplete = fn);

        return () => { 
            active = false; 
            if (unlistenProgress) unlistenProgress();
            if (unlistenComplete) unlistenComplete();
        };
    }, []);

    useEffect(() => {
        let cancelled = false;
        const loadQueueStatus = async () => {
            try {
                const status = await getSoftDeleteQueueStatus();
                if (cancelled) return;
                setDeleteQueueStatus(status || { pending_screenshots: 0, pending_ocr: 0, running: false });
            } catch {
                if (cancelled) return;
                setDeleteQueueStatus({ pending_screenshots: 0, pending_ocr: 0, running: false });
            }
        };

        loadQueueStatus();
        const timer = setInterval(loadQueueStatus, 4000);
        return () => {
            cancelled = true;
            clearInterval(timer);
        };
    }, []);

    useEffect(() => {
        if (!hasDeleteTask) {
            setDeleteQueuePeak(0);
            return;
        }
        if (pendingDeleteTotal > 0) {
            setDeleteQueuePeak((prev) => Math.max(prev, pendingDeleteTotal));
        }
    }, [hasDeleteTask, pendingDeleteTotal]);

    return (
        <div
            className="relative w-[450px] z-50 pointer-events-auto"
            ref={wrapperRef}
            onClick={(e) => e.stopPropagation()}
            data-keep-selection="true"
        >
            <div className="relative flex items-center bg-ide-panel rounded-md border border-ide-border focus-within:border-ide-accent focus-within:ring-1 focus-within:ring-ide-accent transition-all shadow-sm overflow-hidden">
                {hasDeleteTask && (
                    <div
                        className="pointer-events-none absolute inset-y-0 left-0 bg-sky-500/35 dark:bg-sky-400/20 transition-all duration-500"
                        style={{ width: `${progressFillPercent}%` }}
                    />
                )}
                <div className="relative z-10 flex items-center border-r border-ide-border mr-2">
                <button
                    className={`p-2 text-ide-muted hover:text-ide-text transition-colors ${backendOnline === false && mode === 'ocr' ? 'opacity-50 cursor-not-allowed' : ''}`}
                    onClick={() => {
                        // 如果后端离线且当前是 OCR 模式，不允许切换到 NL 模式
                        if (backendOnline === false && mode === 'ocr') return;
                        userInteractionRef.current = true;
                        setMode(mode === 'ocr' ? 'nl' : 'ocr');
                    }}
                    title={backendOnline === false && mode === 'ocr' ? t('search.nl.disabled_hint') : (mode === 'ocr' ? t('search.switchToNL') : t('search.switchToOCR'))}
                >
                    {mode === 'ocr' ? <Type size={16} /> : <ImageIcon size={16} />}
                </button>
                <button
                    className="p-2 pl-0 text-ide-muted hover:text-ide-text transition-colors"
                    onClick={(e) => {
                        e.stopPropagation();
                        console.log('ChevronDown clicked, current showModeMenu:', showModeMenu);
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
                placeholder={hasDeleteTask ? taskSummaryPlaceholder : (mode === 'ocr' ? t('search.placeholder.ocr') : t('search.placeholder.nl'))}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onFocus={() => setShowResults(true)}
                onKeyDown={(event) => {
                    if (event.key === 'Enter') {
                        event.preventDefault();
                        setShowResults(false);
                        if (onSubmit) {
                            onSubmit({ query, mode });
                        }
                    }
                }}
            />
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
                        onClick={() => { userInteractionRef.current = true; setMode('ocr'); setShowModeMenu(false); }}
                    >
                        <div className="mt-1 text-ide-accent"><Type size={18} /></div>
                        <div>
                            <div className="text-sm font-bold text-ide-text">{t('search.mode.ocr.title')}</div>
                            <div className="text-xs text-ide-muted leading-relaxed">{t('search.mode.ocr.description')}</div>
                        </div>
                    </button>
                    <button
                        className={`flex items-start gap-3 p-3 rounded hover:bg-ide-hover text-left transition-colors ${mode === 'nl' ? 'bg-ide-active border border-ide-border' : ''} ${backendOnline === false ? 'opacity-50 cursor-not-allowed' : ''}`}
                        onClick={() => { if (backendOnline === false) return; userInteractionRef.current = true; setMode('nl'); setShowModeMenu(false); }}
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
                        {results.length > 0 ? (
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
                const src = await fetchImage(id, id ? null : path);
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
