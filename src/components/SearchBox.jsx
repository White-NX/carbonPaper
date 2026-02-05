import React, { useState, useEffect, useRef } from 'react';
import { Image as ImageIcon, Type, Loader2, X, ChevronDown } from 'lucide-react';
import { searchScreenshots, fetchImage } from '../lib/monitor_api';

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

export function SearchBox({ onSelectResult, onSubmit, mode: controlledMode, onModeChange }) {
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

    return (
        <div 
            className="relative w-[450px] z-50 pointer-events-auto" 
            ref={wrapperRef}
            onClick={(e) => e.stopPropagation()}
            data-keep-selection="true"
        >
            <div className="flex items-center bg-ide-panel rounded-md border border-ide-border focus-within:border-ide-accent focus-within:ring-1 focus-within:ring-ide-accent transition-all shadow-sm">
                <div className="relative flex items-center border-r border-ide-border mr-2">
                <button
                    className="p-2 text-ide-muted hover:text-ide-text transition-colors"
                    onClick={() => { userInteractionRef.current = true; setMode(mode === 'ocr' ? 'nl' : 'ocr'); }}
                    title={mode === 'ocr' ? "Switch to Natural Language Search" : "Switch to OCR Search"}
                >
                    {mode === 'ocr' ? <Type size={16} /> : <ImageIcon size={16} />}
                </button>
                <button
                    className="p-2 pl-0 text-ide-muted hover:text-ide-text transition-colors"
                    onClick={(e) => { e.stopPropagation(); setShowModeMenu(!showModeMenu); }}
                    title="Select Search Mode"
                >
                    <ChevronDown size={14} />
                </button>
                {showModeMenu && (
                    <div className="absolute top-full left-0 mt-2 w-72 bg-ide-panel border border-ide-border rounded-md shadow-xl z-[60] p-1 flex flex-col gap-1">
                        <button
                            className={`flex items-start gap-3 p-3 rounded hover:bg-ide-hover text-left transition-colors ${mode === 'ocr' ? 'bg-ide-active border border-ide-border' : ''}`}
                            onClick={() => { userInteractionRef.current = true; setMode('ocr'); setShowModeMenu(false); }}
                        >
                            <div className="mt-1 text-ide-accent"><Type size={18} /></div>
                            <div>
                                <div className="text-sm font-bold text-ide-text">OCR 关键词搜索模式</div>
                                <div className="text-xs text-ide-muted leading-relaxed">通过识别屏幕截图中的文字进行精确匹配搜索</div>
                            </div>
                        </button>
                        <button
                            className={`flex items-start gap-3 p-3 rounded hover:bg-ide-hover text-left transition-colors ${mode === 'nl' ? 'bg-ide-active border border-ide-border' : ''}`}
                            onClick={() => { userInteractionRef.current = true; setMode('nl'); setShowModeMenu(false); }}
                        >
                            <div className="mt-1 text-ide-success"><ImageIcon size={18} /></div>
                            <div>
                                <div className="text-sm font-bold text-ide-text">自然语言搜索模式</div>
                                <div className="text-xs text-ide-muted leading-relaxed">描述图片画面内容、场景或物体进行语义模糊搜索</div>
                            </div>
                        </button>
                    </div>
                )}
            </div>

            <input
                ref={inputRef}
                type="text"
                className="bg-transparent border-none outline-none text-ide-text text-sm w-full h-9 px-3 placeholder-ide-muted"
                placeholder={mode === 'ocr' ? "Search text in screenshots..." : "Describe image content..."}
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
                <Loader2 size={16} className="animate-spin text-ide-muted mr-2" />
            ) : (
                query && (
                    <button onClick={() => setQuery('')} className="p-1 mr-2 text-ide-muted hover:text-ide-text">
                        <X size={14} />
                    </button>
                )
            )}
            </div>

            {showResults && (
                <div className="absolute top-full mt-2 w-[450px] bg-ide-panel border border-ide-border rounded-lg shadow-2xl overflow-hidden max-h-[600px] flex flex-col">
                    <div className="overflow-y-auto custom-scrollbar">
                        {results.length > 0 ? (
                            results.map((item, index) => (
                                <SearchResultItem
                                    key={item.id || index}
                                    item={item}
                                    mode={mode}
                                    query={debouncedQuery}
                                    onClick={() => handleSelect(item)}
                                />
                            ))
                        ) : loading ? (
                            <div className="p-4 text-center text-ide-muted text-sm">Loading...</div>
                        ) : (
                            <div className="p-4 text-center text-ide-muted text-sm">No Contents</div>
                        )}
                    </div>
                </div>
            )}
        </div>
    );
}

function SearchResultItem({ item, mode, query, onClick }) {
    const [imgSrc, setImgSrc] = useState(null);

    useEffect(() => {
        let active = true;
        const loadImg = async () => {
            const screenshotId = mode === 'nl' ? item.metadata?.screenshot_id : item.screenshot_id;
            const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
            const path = item.image_path || item.metadata?.image_path || item.path;
            const src = await fetchImage(id, id ? null : path);
            if (active) setImgSrc(src);
        };
        loadImg();
        return () => { active = false; };
    }, [item, mode]);

    // Highlighting logic for OCR
    const renderOCRText = () => {
        const text = mode === 'nl' ? item.ocr_text : item.text;
        if (!text) return <span className="text-ide-muted italic">No text content</span>;

        if (mode === 'nl') {
            return <span className="text-ide-muted font-light">{text.length > 150 ? text.substring(0, 150) + '...' : text}</span>;
        }

        // Simple highlight for OCR keywords
        if (!query) return <span className="text-ide-muted font-light">{text}</span>;

        const parts = text.split(new RegExp(`(${query})`, 'gi'));
        return (
            <span className="text-ide-muted font-light">
                {parts.map((part, i) =>
                    part.toLowerCase() === query.toLowerCase()
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
                ) : (
                    <div className="w-full h-full flex items-center justify-center">
                        <Loader2 size={16} className="animate-spin text-ide-muted" />
                    </div>
                )}
            </div>
            <div className="flex-1 min-w-0 flex flex-col gap-1 py-0.5">
                <div className="text-sm text-ide-text truncate font-bold flex items-baseline">
                    <span className="text-ide-accent mr-2">{processName || 'Unknown Process'}</span>
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
                        Match Score: {(item.similarity || 0).toFixed(2)}
                    </div>
                )}
            </div>
        </div>
    );
}
