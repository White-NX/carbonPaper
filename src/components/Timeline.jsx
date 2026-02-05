import React, { useState, useRef, useEffect, useMemo, useCallback } from 'react';
import { getTimeline, fetchTimelineImage, clearTimelineImageQueue, cancelTimelineImageRequest } from '../lib/monitor_api';
import { Locate, Play } from 'lucide-react';

// Simple debounce
function simpleDebounce(func, wait) {
    let timeout;
    return function(...args) {
        const context = this;
        clearTimeout(timeout);
        timeout = setTimeout(() => func.apply(context, args), wait);
    };
}

// Simple throttle (leading edge only, good for continuous updates like follow mode)
function simpleThrottle(func, limit) {
    let lastRun = 0;
    return function(...args) {
        const now = Date.now();
        if (now - lastRun >= limit) {
            func.apply(this, args);
            lastRun = now;
        }
    };
}

// Generate a unique key for an activity segment (combines process + window)
const getActivityKey = (appName, windowTitle) => {
    return `${appName || ''}::${windowTitle || ''}`;
};

const getProcessColor = (processName, windowTitle = '', prevProcessName = null, prevWindowTitle = null) => {
    if (!processName) return '#888';
    
    // Use both process name and window title for color generation
    // This ensures different windows of the same app get different colors
    const colorKey = `${processName}::${windowTitle || ''}`;
    let hash = 0;
    for (let i = 0; i < colorKey.length; i++) {
        hash = colorKey.charCodeAt(i) + ((hash << 5) - hash);
    }
    let hue = Math.abs(hash % 360);
    
    // Ensure adjacent activities have different colors
    if (prevProcessName !== null) {
        const prevColorKey = `${prevProcessName || ''}::${prevWindowTitle || ''}`;
        if (prevColorKey !== colorKey) {
            let prevHash = 0;
            for (let i = 0; i < prevColorKey.length; i++) {
                prevHash = prevColorKey.charCodeAt(i) + ((prevHash << 5) - prevHash);
            }
            const prevHue = Math.abs(prevHash % 360);
            // If hues are too similar (within 40 degrees), offset by 60 degrees
            const hueDiff = Math.abs(hue - prevHue);
            if (hueDiff < 40 || hueDiff > 320) {
                hue = (prevHue + 60 + Math.abs(hash % 120)) % 360;
            }
        }
    }
    
    return `hsl(${hue}, 65%, 40%)`; 
};

const TIMELINE_IMAGE_CACHE_LIMIT = 800;
const timelineImageCache = new Map();

const getTimelineImageCacheKey = (event) => {
    if (!event) return null;
    return event.id ?? event.imagePath ?? null;
};

const setTimelineImageCache = (key, dataUrl) => {
    if (key === null || key === undefined || !dataUrl) return;
    if (!timelineImageCache.has(key) && timelineImageCache.size >= TIMELINE_IMAGE_CACHE_LIMIT) {
        const oldestKey = timelineImageCache.keys().next().value;
        timelineImageCache.delete(oldestKey);
    }
    timelineImageCache.set(key, dataUrl);
};

// Sub-component for individual events to handle lazy loading
const TimelineEvent = React.memo(({ event, x, width, visible, showImage, showText, showLabel, isSameActivityAsNext, isSameActivityAsPrev, prevAppName, prevWindowTitle, onClick, isHighlighted, imageEpoch }) => {
    const [imageUrl, setImageUrl] = useState(null);
    const [loading, setLoading] = useState(false);
    const [retryToken, setRetryToken] = useState(0);
    const requestIdRef = useRef(0);
    const loadingRef = useRef(false);
    const retryTimerRef = useRef(null);
    const retryCountRef = useRef(0);

    useEffect(() => {
        let cancelled = false;
        const cacheKey = getTimelineImageCacheKey(event);
        const scheduleRetry = (delayMs) => {
            if (retryTimerRef.current) return;
            retryTimerRef.current = setTimeout(() => {
                retryTimerRef.current = null;
                setRetryToken((value) => value + 1);
            }, delayMs);
        };
        // If highlighted, force load image even if density says no?
        // Actually density logic happens in parent. If parent says showImage=false, we don't render this block.
        // We should ensure parent sets showImage=true for highlighted events.
        if (!visible || !showImage) {
            cancelTimelineImageRequest(cacheKey);
            return () => {
                cancelled = true;
            };
        }

        if (visible && showImage) {
            const cached = cacheKey ? timelineImageCache.get(cacheKey) : null;
            if (cached && cached !== imageUrl) {
                setImageUrl(cached);
            } else if (!imageUrl && !loadingRef.current) {
                loadingRef.current = true;
                setLoading(true);
                const requestId = requestIdRef.current + 1;
                requestIdRef.current = requestId;
                fetchTimelineImage(event.id, event.imagePath, { priority: 'high', key: cacheKey })
                    .then((data) => {
                        if (cancelled || requestIdRef.current !== requestId) return;
                        if (data) {
                            setTimelineImageCache(cacheKey, data);
                            setImageUrl(data);
                        }
                        retryCountRef.current = 0;
                    })
                    .catch((err) => {
                        if (cancelled || requestIdRef.current !== requestId) return;
                        if (err?.code === 'not_found') {
                            return;
                        }
                        if (visible && showImage && !imageUrl) {
                            const nextRetry = Math.min(retryCountRef.current + 1, 5);
                            retryCountRef.current = nextRetry;
                            const delayMs = err?.message === 'cancelled' ? 200 : 400 * nextRetry;
                            scheduleRetry(delayMs);
                        }
                    })
                    .finally(() => {
                        if (cancelled || requestIdRef.current !== requestId) return;
                        loadingRef.current = false;
                        setLoading(false);
                    });
            }
        }
        return () => {
            cancelled = true;
            loadingRef.current = false;
            if (retryTimerRef.current) {
                clearTimeout(retryTimerRef.current);
                retryTimerRef.current = null;
            }
        };
    }, [visible, showImage, event.id, event.imagePath, imageEpoch, imageUrl, retryToken]);

    const iconSrc = useMemo(() => {
        if (!event.processIcon) return null;
        return event.processIcon.startsWith('data:') ? event.processIcon : `data:image/png;base64,${event.processIcon}`;
    }, [event.processIcon]);

    // Cleanup object URL if we were using blobs (but we are using base64 strings)
    
    // Determine width of the "process bar segment"
    let segmentWidth = 100; // Default
    // Logic moved from parent: parent passes calculated params? 
    // Actually the parent loop logic was better for segment width. 
    // Let's rely on the passed `width` prop which is the segment width.

    const processColor = getProcessColor(event.appName, event.windowTitle, prevAppName, prevWindowTitle);
    // barStyle removed - rendered via Canvas

    return (
        <>
            {/* Process Info Bar Segment - REMOVED, using Canvas in parent */}

            {/* Process Label/Icon - Only on start of sequence and when density allows */}
            {(showLabel && !isSameActivityAsPrev) && (
                <div 
                    className="absolute top-0 flex items-center gap-1.5 text-xs z-20 pl-1 -translate-y-1"
                    style={{ left: x, color: processColor }}
                >
                    {iconSrc ? (
                        <img
                            src={iconSrc}
                            alt={event.appName || 'app'}
                            className="w-5 h-5 rounded-sm shadow-sm border border-current object-cover"
                        />
                    ) : (
                        <span className="text-sm bg-ide-panel rounded-full w-5 h-5 flex items-center justify-center shadow-sm border border-current text-[10px] overflow-hidden">
                            {event.appName ? event.appName[0].toUpperCase() : '?'}
                        </span>
                    )}
                    {showText && (
                        <>
                            <span className="font-bold drop-shadow-sm whitespace-nowrap">{event.appName}</span>
                            <span className="opacity-70 text-[10px] whitespace-nowrap overflow-hidden max-w-[150px] text-ellipsis hidden sm:block">
                                {event.windowTitle}
                            </span>
                        </>
                    )}
                </div>
            )}

            {/* Screenshot Node */}
            {showImage && (
                <div 
                    className={`absolute top-4 flex flex-col items-center group hover:z-30 cursor-pointer ${isHighlighted ? 'z-40' : ''}`}
                    style={{ left: x, transform: 'translateX(-50%)' }}
                    onClick={(e) => {
                        e.stopPropagation();
                        if (onClick) onClick(event);
                    }}
                >
                    <div 
                        className="w-0.5 mt-1.5 mb-1 group-hover:h-8 transition-all shadow-sm opacity-80"
                        style={{ 
                            height: '12px', 
                            backgroundColor: processColor 
                        }}
                    ></div>
                    
                    <div className={`bg-ide-panel border p-1 rounded hover:scale-125 transition-transform shadow-lg z-0 relative ${isHighlighted ? 'border-yellow-400 ring-2 ring-yellow-400 ring-opacity-50 scale-125' : 'border-ide-border'}`}>
                        <div className="w-12 h-12 bg-ide-active overflow-hidden rounded-sm relative flex items-center justify-center">
                            {imageUrl ? (
                                <img src={imageUrl} alt={event.title} className="w-full h-full object-cover pointer-events-none" />
                            ) : (
                                <div className="text-ide-muted text-[8px]">...</div>
                            )}
                        </div>
                    </div>
                    
                    <div className={`opacity-0 group-hover:opacity-100 absolute top-full mt-1 bg-ide-panel text-xs text-ide-text px-2 py-1 rounded shadow border border-ide-border whitespace-nowrap z-20 pointer-events-none ${isHighlighted ? 'opacity-100' : ''}`}>
                        {new Date(event.timestamp).toLocaleString()}
                    </div>
                </div>
            )}
        </>
    );
});

const Timeline = ({ onSelectEvent, onClearHighlight, jumpTimestamp, highlightedEventId, refreshKey }) => {
    const containerRef = useRef(null);
    const canvasRef = useRef(null);
    const wheelIdleTimerRef = useRef(null);
    const [width, setWidth] = useState(0);
    const [events, setEvents] = useState([]);
    const [imageEpoch, setImageEpoch] = useState(0);
    
    // View State
    const [centerTime, setCenterTime] = useState(Date.now());
    
    // Zoom levels (pixels per millisecond)
    const MIN_ZOOM = 100 / (365 * 24 * 3600000); // 100px per Year
    // Target: 10px per Second = 10/1000 = 0.01
    const MAX_ZOOM = 20 / 1000; 
    
    // Initialize with a reasonable zoom level (Visible range ~30 minutes)
    // 30 min = 1800000 ms. If width is 1000px, Zoom = 1000 / 1800000 = 0.00055
    const [zoom, setZoom] = useState(0.001); 

    const [isDragging, setIsDragging] = useState(false);
    const lastMouseXRef = useRef(0);
    const isDraggingRef = useRef(false);
    const [isFollowingNow, setIsFollowingNow] = useState(false);

    // Initial width detection
    useEffect(() => {
        if (!containerRef.current) return;
        setWidth(containerRef.current.clientWidth);
        const observer = new ResizeObserver(entries => {
            for (let entry of entries) {
                setWidth(entry.contentRect.width);
            }
        });
        observer.observe(containerRef.current);
        return () => observer.disconnect();
    }, []);

    // Handle jump request
    useEffect(() => {
        if (jumpTimestamp?.time) {
            setIsFollowingNow(false);
            setCenterTime(jumpTimestamp.time);
            // Auto-zoom to a clear level if we are too zoomed out
            setZoom(prev => Math.max(prev, 0.005)); 
            clearTimelineImageQueue();
            setImageEpoch(prev => prev + 1);
        }
    }, [jumpTimestamp]);

    // Follow "Now" logic
    useEffect(() => {
        let animationFrameId;
        
        const tick = () => {
            if (isFollowingNow) {
                setCenterTime(Date.now());
                animationFrameId = requestAnimationFrame(tick);
            }
        };

        if (isFollowingNow) {
            tick();
        }

        return () => {
             if (animationFrameId) cancelAnimationFrame(animationFrameId);
        };
    }, [isFollowingNow]);

    const handleNowClick = () => {
        setCenterTime(Date.now());
        setZoom(MAX_ZOOM); // Zoom to max (seconds view)
        setIsFollowingNow(true);
        clearTimelineImageQueue();
        setImageEpoch(prev => prev + 1);
    };

    // Raw fetch function
    const getEventKey = useCallback((event) => {
        if (!event) return null;
        if (event.id !== null && event.id !== undefined) return event.id;
        if (event.imagePath) return event.imagePath;
        const ts = event.timestamp ?? 0;
        return `${ts}-${event.appName || ''}-${event.windowTitle || ''}`;
    }, []);

    const fetchEventsRaw = async (center, currentZoom, containerWidth) => {
        if (!containerWidth) return;
        
        const timeSpan = containerWidth / currentZoom;
        const startTime = center - (timeSpan / 2) - (timeSpan * 0.5); 
        const endTime = center + (timeSpan / 2) + (timeSpan * 0.5);
        
        try {
            const records = await getTimeline(startTime, endTime);
            console.log('[Timeline] Raw records from API:', records);
            const mapped = (records || [])
                .filter(r => r.timestamp != null) // Filter out records without timestamp
                .map(r => {
                    let meta = null;
                    if (r?.metadata) {
                        try {
                            meta = typeof r.metadata === 'string' ? JSON.parse(r.metadata) : r.metadata;
                        } catch (e) {
                            meta = null;
                        }
                    }

                    return {
                        id: r.id,
                        timestamp: r.timestamp ? r.timestamp * 1000 : (r.created_at ? new Date(r.created_at).getTime() : 0),
                        imagePath: r.image_path,
                        appName: r.process_name,
                        windowTitle: r.window_title,
                        processIcon: r.process_icon || meta?.process_icon || null,
                        processPath: r.process_path || meta?.process_path || null,
                    };
                })
                .filter(e => !isNaN(e.timestamp)); // Filter out invalid timestamps
            console.log('[Timeline] Mapped events:', mapped.length);
            
            setEvents(prev => {
                const combined = [...mapped, ...prev];
                const seen = new Set();
                const unique = [];
                for (const e of combined) {
                    const key = getEventKey(e);
                    if (!seen.has(key)) {
                        seen.add(key);
                        unique.push(e);
                    }
                }
                const sorted = unique.sort((a, b) => a.timestamp - b.timestamp);
                console.log('[Timeline] Total unique events:', sorted.length);
                return sorted;
            });
        } catch (err) {
            console.error('[Timeline] Fetch error:', err);
        }
    };

    // Create memoized debounced and throttled versions
    const fetchEventsDebounced = useMemo(() => simpleDebounce(fetchEventsRaw, 500), []);
    const fetchEventsThrottled = useMemo(() => simpleThrottle(fetchEventsRaw, 1000), []);

    // One-shot refresh (e.g., after delete)
    useEffect(() => {
        if (refreshKey === undefined) return;
        setEvents([]);
        clearTimelineImageQueue();
        setImageEpoch(prev => prev + 1);
        fetchEventsRaw(centerTime, zoom, width);
    }, [refreshKey]);

    // Main interaction effect
    useEffect(() => {
        if (isFollowingNow) {
            fetchEventsThrottled(centerTime, zoom, width);
        } else {
            fetchEventsDebounced(centerTime, zoom, width);
        }
    }, [centerTime, zoom, width, isFollowingNow, fetchEventsDebounced, fetchEventsThrottled]);

    // Periodic refresh for static view (every 5s)
    useEffect(() => {
        const interval = setInterval(() => {
            // Only refresh if NOT following now (following handles itself) 
            // and NOT currently dragging (avoid jank)
            if (!isFollowingNow && !isDragging) {
                // Use debounce version to avoid conflict
                fetchEventsDebounced(centerTime, zoom, width);
            }
        }, 5000);
        return () => clearInterval(interval);
    }, [isFollowingNow, isDragging, centerTime, zoom, width, fetchEventsDebounced]);


    // Interaction Handlers
    const handleMouseDown = (e) => {
        setIsFollowingNow(false);
        setIsDragging(true);
        isDraggingRef.current = true;
        lastMouseXRef.current = e.clientX;
        clearTimelineImageQueue();
    };

    const handleMouseMove = (e) => {
        if (!isDraggingRef.current) return;
        const deltaX = e.clientX - lastMouseXRef.current;
        lastMouseXRef.current = e.clientX;
        const deltaTime = deltaX / zoom; 
        setCenterTime(prev => prev - deltaTime);
    };

    const handleMouseUp = () => {
        setIsDragging(false);
        isDraggingRef.current = false;
        setImageEpoch(prev => prev + 1);
    };

    const handleMouseLeave = () => {
        setIsDragging(false);
        isDraggingRef.current = false;
        setImageEpoch(prev => prev + 1);
    };

    const handleBackgroundClick = useCallback((e) => {
        // Only clear highlight when clicking on the empty canvas (not dragging and not on child nodes)
        if (isDragging) return;
        if (e.target !== e.currentTarget) return;
        setIsFollowingNow(false);
        if (onClearHighlight) onClearHighlight();
    }, [isDragging, onClearHighlight]);

    const handleWheel = (e) => {
        try { e.preventDefault(); } catch(err){} 
        setIsFollowingNow(false);
        clearTimelineImageQueue();

        if (wheelIdleTimerRef.current) {
            clearTimeout(wheelIdleTimerRef.current);
        }
        wheelIdleTimerRef.current = setTimeout(() => {
            setImageEpoch(prev => prev + 1);
        }, 160);

        const rect = containerRef.current.getBoundingClientRect();
        const cursorX = e.clientX - rect.left;
        // Calculate the time at the mouse cursor BEFORE zooming
        const timeAtCursor = centerTime + (cursorX - width / 2) / zoom;
        
        const zoomFactor = 1.2; 
        let newZoom = zoom;
        if (e.deltaY < 0) newZoom *= zoomFactor;
        else newZoom /= zoomFactor;
        
        // Clamp zoom
        newZoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, newZoom));
        
        // Calculate new center time so that timeAtCursor remains at cursorX
        // cursorX = width/2 + (timeAtCursor - newCenterTime) * newZoom
        // => (cursorX - width/2) / newZoom = timeAtCursor - newCenterTime
        // => newCenterTime = timeAtCursor - (cursorX - width/2) / newZoom
        const newCenterTime = timeAtCursor - (cursorX - width / 2) / newZoom;

        setZoom(newZoom);
        setCenterTime(newCenterTime);
    };

    const getXPosition = (timestamp) => {
        const timeDiff = timestamp - centerTime;
        return (width / 2) + (timeDiff * zoom);
    };

    // Ticks Rendering (Smart scale)
    const formatTick = (date, stepMs) => {
        const day = 86400000;
        // Year-level ticks
        if (stepMs >= day * 365) return `${date.getFullYear()}`;
        // Month-level ticks (show month + year to avoid month-only repetition)
        if (stepMs >= day * 28) return date.toLocaleString('default', { month: 'short', year: 'numeric' });
        // Day-level ticks
        if (stepMs >= day) return date.toLocaleString('default', { month: 'short', day: 'numeric' });
        
        const h = date.getHours();
        const m = String(date.getMinutes()).padStart(2, '0');
        const s = String(date.getSeconds()).padStart(2, '0');
        const ms = String(date.getMilliseconds()).padStart(3, '0');

        if (stepMs >= 3600000) return `${h}:00`;
        if (stepMs >= 60000) return `${h}:${m}`;
        if (stepMs >= 1000) return `${h}:${m}:${s}`;
        return `${h}:${m}:${s}.${ms}`;
    };

    const getTickStep = (currentZoom) => {
        const minSpacing = 120; // px
        const targetMs = minSpacing / currentZoom;
        const day = 86400000;

        // Nice intervals from milliseconds up to multi-year spans
        const steps = [
            10, 20, 50, 100, 200, 500, // ms
            1000, 2000, 5000, 10000, 15000, 30000, // seconds
            60000, 120000, 300000, 900000, 1800000, // minutes
            3600000, 7200000, 21600000, 43200000, // hours
            day, day * 2, day * 7, day * 30, day * 90, day * 180, // days to months
            day * 365, day * 365 * 2, day * 365 * 5, day * 365 * 10, // years
            day * 365 * 25, day * 365 * 50, day * 365 * 100, day * 365 * 250, day * 365 * 500, day * 365 * 1000, day * 365 * 2500
        ];

        return steps.find(s => s >= targetMs) || steps[steps.length - 1];
    };

    const tickStepMs = useMemo(() => getTickStep(zoom), [zoom]);
    const showProcessText = tickStepMs < 900000; // Show names only when finer than 15 minutes to avoid text stacking

    // Canvas drawing for the timeline activity bars
    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas || !width) return;
        
        const dpr = window.devicePixelRatio || 1;
        const rect = containerRef.current.getBoundingClientRect();
        // Keep CSS size in layout pixels to avoid visual scaling mismatch
        if (canvas.style.width !== `${rect.width}px`) {
            canvas.style.width = `${rect.width}px`;
            canvas.style.height = `${rect.height}px`;
        }
        
        // Ensure accurate scaling
        if (canvas.width !== rect.width * dpr || canvas.height !== rect.height * dpr) {
            canvas.width = rect.width * dpr;
            canvas.height = rect.height * dpr;
        }

        const ctx = canvas.getContext('2d');
        ctx.resetTransform(); // clear previous transforms
        ctx.scale(dpr, dpr);
        ctx.clearRect(0, 0, rect.width, rect.height);

        if (events.length === 0) return;

        const startTime = centerTime - (width / 2) / zoom;
        const endTime = centerTime + (width / 2) / zoom;

        // Binary search for visible range
        // Find index where event.timestamp >= startTime
        let startIndex = 0;
        let low = 0, high = events.length - 1;
        while (low <= high) {
            const mid = Math.floor((low + high) / 2);
            if (events[mid].timestamp < startTime) low = mid + 1;
            else high = mid - 1;
        }
        startIndex = Math.max(0, low - 1); 

        // Find index where event.timestamp > endTime
        let endIndex = events.length;
        low = 0; high = events.length - 1;
        while (low <= high) {
            const mid = Math.floor((low + high) / 2);
            if (events[mid].timestamp <= endTime) low = mid + 1;
            else high = mid - 1;
        }
        endIndex = Math.min(events.length, low + 1);

        const getLocalX = (ts) => (width / 2) + ((ts - centerTime) * zoom);

        for (let i = startIndex; i < endIndex; i++) {
             const event = events[i];
             const nextEvent = events[i+1]; // events[endIndex] might be undefined, handled safe
             
             const x = getLocalX(event.timestamp);
             let segmentWidth = 100;
             let nextX = x + 100;

             if (nextEvent) {
                  nextX = getLocalX(nextEvent.timestamp);
                  segmentWidth = Math.max(0, nextX - x);
             } else {
                 // Last event: Arbitrary width or based on duration if known? 
                 // Current logic assumes 100 or till next
             }
             
             // Culling (extra check)
             if (x > width || x + segmentWidth < 0) continue;

             const currentActivityKey = getActivityKey(event.appName, event.windowTitle);
             const nextActivityKey = nextEvent ? getActivityKey(nextEvent.appName, nextEvent.windowTitle) : null;
             const isSameActivityAsNext = nextEvent && nextActivityKey === currentActivityKey;
             
             // Only calculate color for visible bars
             const prevEvent = i > 0 ? events[i-1] : null;
             const prevAppName = prevEvent?.appName;
             const prevWindowTitle = prevEvent?.windowTitle;
             
             ctx.fillStyle = getProcessColor(event.appName, event.windowTitle, prevAppName, prevWindowTitle);
             
             const barTop = 16; 
             const barHeight = 6; 
             const arrowDepth = 8;
             
             if (isSameActivityAsNext) {
                 ctx.fillRect(x, barTop, segmentWidth + 1, barHeight);
             } else {
                 ctx.beginPath();
                 ctx.moveTo(x, barTop);
                 ctx.lineTo(x + segmentWidth - arrowDepth, barTop);
                 ctx.lineTo(x + segmentWidth, barTop + barHeight / 2);
                 ctx.lineTo(x + segmentWidth - arrowDepth, barTop + barHeight);
                 ctx.lineTo(x, barTop + barHeight);
                 ctx.closePath();
                 ctx.fill();
             }
        }
    }, [events, centerTime, zoom, width]);

    const renderTicks = () => {
        if (!width) return null;
        
        const stepMs = tickStepMs;
        const startTime = centerTime - (width / 2) / zoom;
        const endTime = centerTime + (width / 2) / zoom;
        
        // Align to step
        let currentTime = Math.floor(startTime / stepMs) * stepMs;
        
        const ticks = [];
        let count = 0;
        
        while (currentTime < endTime && count < 100) {
            const x = getXPosition(currentTime);
            // Only render if within reasonable bounds (add slight buffer)
            if (x > -50 && x < width + 50) {
                const date = new Date(currentTime);
                ticks.push(
                    <div key={currentTime} className="absolute bottom-0 border-l border-ide-muted h-4 opacity-50 flex flex-col items-start pointer-events-none" style={{ left: x }}>
                         <span className="text-xs text-ide-muted ml-1 whitespace-nowrap select-none -translate-x-1/2 font-mono">
                            {formatTick(date, stepMs)}
                        </span>
                    </div>
                );
            }
            currentTime += stepMs;
            count++;
        }
        return ticks;
    };
    
    return (
        <div 
            ref={containerRef}
            className="w-full h-32 bg-ide-bg border-b border-ide-border relative overflow-hidden cursor-move select-none shadow-inner"
            onMouseDown={handleMouseDown}
            onMouseMove={handleMouseMove}
            onMouseUp={handleMouseUp}
            onMouseLeave={handleMouseLeave}
            onWheel={handleWheel}
            onClick={handleBackgroundClick}
            data-keep-selection="true"
        >
            {renderTicks()}
            <canvas ref={canvasRef} className="absolute inset-0 z-0 pointer-events-none" />

            {width > 0 && (() => {
                const startTime = centerTime - (width / 2) / zoom;
                const endTime = centerTime + (width / 2) / zoom;
                
                // Binary Search for Visible Range Logic (Same as Canvas)
                // Find index where event.timestamp >= startTime
                let startIndex = 0;
                let low = 0, high = events.length - 1;
                while (low <= high) {
                    const mid = Math.floor((low + high) / 2);
                    if (events[mid].timestamp < startTime) low = mid + 1;
                    else high = mid - 1;
                }
                startIndex = Math.max(0, low - 1); 

                // Find index where event.timestamp > endTime
                let endIndex = events.length;
                low = 0; high = events.length - 1;
                while (low <= high) {
                    const mid = Math.floor((low + high) / 2);
                    if (events[mid].timestamp <= endTime) low = mid + 1;
                    else high = mid - 1;
                }
                endIndex = Math.min(events.length, low + 1);
                let lastImageX = -9999;
                let lastLabelX = -9999;
                const MIN_IMAGE_GAP = 20;
                const isFineZoom = tickStepMs < 30000;
                const MIN_LABEL_GAP = showProcessText ? 180 : (isFineZoom ? 0 : 30);

                // Sparse sampling on macro scales without changing the visual placement rules.
                const visibleSpanMs = Math.max(1, endTime - startTime);
                const macroScale = tickStepMs > 120000; // coarser than 2 min/tick
                const maxImagesPerView = macroScale
                    ? Math.max(14, Math.min(60, Math.floor(width / 60)))
                    : null;
                const sampleIntervalMs = macroScale && maxImagesPerView
                    ? Math.max(tickStepMs * 1.5, Math.floor(visibleSpanMs / maxImagesPerView))
                    : null;
                const seenSampleBuckets = macroScale ? new Set() : null;
                
                const visibleNodes = [];

                for (let index = startIndex; index < endIndex; index++) {
                    const event = events[index];
                    // We access original events array for neighbors to maintain continuity
                    const nextEvent = events[index + 1];
                    const prevEvent = index > 0 ? events[index - 1] : null;
                    const x = getXPosition(event.timestamp);
                    
                    let segmentWidth = 100;
                    if (nextEvent) {
                        const nextX = getXPosition(nextEvent.timestamp);
                        segmentWidth = Math.max(0, nextX - x);
                    }

                    // Strict Culling (should be duplicate of binary search mostly, but good for safety)
                    if (x > width + 50) break; 
                    if (x + segmentWidth < -50) continue;

                    const currentActivityKey = getActivityKey(event.appName, event.windowTitle);
                    const nextActivityKey = nextEvent ? getActivityKey(nextEvent.appName, nextEvent.windowTitle) : null;
                    const prevActivityKey = prevEvent ? getActivityKey(prevEvent.appName, prevEvent.windowTitle) : null;
                    
                    const isSameActivityAsNext = nextEvent && nextActivityKey === currentActivityKey;
                    const isSameActivityAsPrev = prevEvent && prevActivityKey === currentActivityKey;
                    // Density check for images
                    let showImage = false;
                    const isZoomedEnough = zoom > 0.00001;
                    let isSampled = true;
                    let sampleBucket = null;
                    if (macroScale && sampleIntervalMs) {
                        sampleBucket = Math.floor(event.timestamp / sampleIntervalMs);
                        if (seenSampleBuckets.has(sampleBucket)) {
                            isSampled = false;
                        }
                    }
                    if (isZoomedEnough && isSampled) {
                        if (x - lastImageX >= MIN_IMAGE_GAP) {
                            showImage = true;
                            lastImageX = x;
                            if (sampleBucket !== null) {
                                seenSampleBuckets.add(sampleBucket);
                            }
                        }
                    }
                    // Density check for labels
                    let showLabel = false;
                    if (!isSameActivityAsPrev) {
                        if (x - lastLabelX >= MIN_LABEL_GAP) {
                            showLabel = true;
                            lastLabelX = x;
                        }
                    }

                    const isHighlighted = highlightedEventId === event.id;
                    if (isHighlighted) {
                        showImage = true;
                    }

                    // Optimization: If nothing to render (no image, no label, bar is on canvas), skip component entirely
                    if (!showImage && !(showLabel && !isSameActivityAsPrev)) {
                        continue;
                    }

                    const eventKey = getEventKey(event) ?? `${event.timestamp}-${index}`;
                    visibleNodes.push(
                        <TimelineEvent 
                            key={eventKey}
                            event={event}
                            x={x}
                            width={segmentWidth}
                            visible={true} 
                            showImage={showImage}
                            showText={showProcessText}
                            showLabel={showLabel}
                            isSameActivityAsNext={isSameActivityAsNext}
                            isSameActivityAsPrev={isSameActivityAsPrev}
                            prevAppName={prevEvent?.appName}
                            prevWindowTitle={prevEvent?.windowTitle}
                            onClick={onSelectEvent}
                            isHighlighted={isHighlighted}
                            imageEpoch={imageEpoch}
                        />
                    );
                }
                return visibleNodes;
            })()}
            
            <div className="absolute top-0 bottom-0 left-1/2 w-px bg-ide-accent opacity-50 pointer-events-none z-0"></div>
            
             {/* Now Button */}
            <button 
                className={`absolute bottom-2 right-2 p-1.5 rounded-full shadow-lg border transition-all z-50
                    ${isFollowingNow 
                        ? 'bg-ide-accent text-white border-ide-accent' 
                        : 'bg-ide-panel text-ide-text border-ide-border hover:bg-ide-active'
                    }`}
                onClick={(e) => { e.stopPropagation(); handleNowClick(); }}
                title="Jump to Now"
            >
                <Play size={16} fill={isFollowingNow ? "currentColor" : "none"} className={isFollowingNow ? "" : "ml-0.5"} />
            </button>
        </div>
    );
};

export default Timeline;
