import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import PropTypes from 'prop-types';
import {
  AlertTriangle,
  ArrowLeftRight,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Copy,
  Edit,
  ExternalLink,
  EyeOff,
  Loader2,
  Maximize2,
  RefreshCw,
  Square,
  Trash2,
  ListChecks,
  ShieldCheck,
} from 'lucide-react';
import { cn, getApiBase } from '../lib/utils';
import { ConfirmDialog } from './ConfirmDialog';
import { ImageCompareDialog } from './ImageCompareDialog';
import { Dialog } from './Dialog';

const SAFE_MODE_LEVELS = {
  close: 'close',
  enable: 'enable',
  strict: 'strict',
};

const MILD_LABELS = new Set([
  'FACE_FEMALE',
  'FACE_MALE',
  'ARMPITS_COVERED',
  'ARMPITS_EXPOSED',
  'FEMALE_BREAST_COVERED',
  'MALE_BREAST_EXPOSED',
  'BELLY_COVERED',
  'BELLY_EXPOSED',
  'FEET_COVERED',
  'FEET_EXPOSED',
]);

const isSafeLabel = (label = '') => {
  const normalized = label.toUpperCase();
  return normalized.includes('FACE') || normalized.includes('ARMPITS') || normalized.includes('FEET');
};

const isMildLabel = (label = '') => {
  const normalized = label.toUpperCase();
  return normalized.includes('MILD') || normalized.includes('SAFE') || normalized.includes('COVERED') || MILD_LABELS.has(normalized);
};

const clamp01 = (value) => {
  if (!Number.isFinite(value)) return 0;
  if (value < 0) return 0;
  if (value > 1) return 1;
  return value;
};


const buildImageUrl = (image, options = {}) => {
  if (!image?.filename) return null;
  const params = new URLSearchParams({
    filename: image.filename,
    subfolder: image.subfolder || '',
    type: image.type || 'temp',
    ...options,
  });
  return `${getApiBase()}/view?${params.toString()}`;
};

const normalizeModeration = (raw) => {
  if (!raw) {
    return { detections: [], severity: 'none' };
  }

  const boxes = Array.isArray(raw.boxes) ? raw.boxes : [];

  const detections = boxes.map((entry, index) => {
    const rawLabel = entry.label || entry.class || entry.name || '';
    const label = rawLabel ? rawLabel.toString().toUpperCase() : `UNKNOWN_${index}`;
    const confidence = entry.confidence ?? entry.score ?? 0;
    const [x, y, width, height] = Array.isArray(entry.box) ? entry.box : [];
    
    const isPixel = x > 1 || y > 1 || width > 1 || height > 1;
    
    const box = {
      x: x || 0,
      y: y || 0,
      width: width || 0,
      height: height || 0,
      unit: isPixel ? 'pixel' : 'normalized',
    };
    const isSafe = isSafeLabel(label);
    const isSensitive = entry.isSensitive ?? (!isMildLabel(label) && !isSafe);

    return {
      id: `${label}-${index}`,
      label,
      confidence,
      box,
      isSensitive,
      isSafe,
    };
  });

  const severity =
    raw.severity ||
    (detections.some((det) => det.isSensitive)
      ? 'sensitive'
      : detections.some((det) => !det.isSafe)
      ? 'mild'
      : 'none');

  return {
    detections,
    severity,
  };
};

const groupDetections = (detections = []) =>
  detections.reduce(
    (acc, detection) => {
      if (detection.isSafe) return acc;
      const bucket = detection.isSensitive && !isMildLabel(detection.label) ? 'sensitive' : 'mild';
      acc[bucket].push(detection);
      return acc;
    },
    { mild: [], sensitive: [] }
  );

const buildBlurConfig = ({ safeMode, selectiveAmbiguity, moderation, hasModeration }) => {
  const defaultConfig = {
    shouldBlurThumbnail: false,
    shouldBlurInspector: false,
    inspectorBoxes: [],
  };

  if (safeMode === SAFE_MODE_LEVELS.close) {
    return defaultConfig;
  }

  if (!hasModeration) {
    return {
      shouldBlurThumbnail: true,
      shouldBlurInspector: true,
      inspectorBoxes: [],
    };
  }

  const detections = (moderation?.detections || []).filter((d) => !d.isSafe);
  const sensitiveDetections = detections.filter(
    (det) => det.isSensitive && !isMildLabel(det.label)
  );
  const severity =
    moderation?.severity ||
    (sensitiveDetections.length ? 'sensitive' : detections.length ? 'mild' : 'none');

  if (selectiveAmbiguity && detections.length > 0) {
    const boxesToMask = sensitiveDetections.length > 0 ? sensitiveDetections : detections;
    return {
      shouldBlurThumbnail:
        safeMode === SAFE_MODE_LEVELS.strict || severity === 'sensitive',
      shouldBlurInspector: false,
      inspectorBoxes: boxesToMask,
    };
  }

  if (safeMode === SAFE_MODE_LEVELS.enable) {
    const shouldBlur = severity === 'sensitive';
    return {
      shouldBlurThumbnail: shouldBlur,
      shouldBlurInspector: shouldBlur,
      inspectorBoxes: [],
    };
  }

  if (safeMode === SAFE_MODE_LEVELS.strict) {
    return {
      shouldBlurThumbnail: true,
      shouldBlurInspector: true,
      inspectorBoxes: [],
    };
  }

  return defaultConfig;
};

const useGroupedHistory = (history) =>
  useMemo(() => {
    if (!history?.length) return [];

    // Deduplicate history by promptId to prevent display issues
    const uniqueHistory = Array.from(
      new Map(history.map((item) => [item.promptId, item])).values()
    );

    const groupMap = uniqueHistory.reduce((acc, item) => {
      const timestamp = item.completedAt || item.createdAt || Date.now();
      const date = new Date(timestamp);
      
      // Use local time for grouping to match user's perspective and avoid UTC splitting
      const year = date.getFullYear();
      const month = String(date.getMonth() + 1).padStart(2, '0');
      const day = String(date.getDate()).padStart(2, '0');
      const key = `${year}-${month}-${day}`;

      if (!acc[key]) {
        acc[key] = {
          id: key,
          label: date.toLocaleDateString(),
          timestamp: new Date(year, date.getMonth(), date.getDate()).getTime(),
          items: [],
        };
      }
      acc[key].items.push(item);
      return acc;
    }, {});

    return Object.values(groupMap)
      .sort((a, b) => b.timestamp - a.timestamp)
      .map((group) => ({
        id: group.id,
        dateLabel: group.label,
        items: group.items.sort(
          (a, b) =>
            (b.completedAt || b.createdAt || 0) - (a.completedAt || a.createdAt || 0)
        ),
      }));
  }, [history]);

const findFirstSensitiveItem = (items) =>
  items.find((item) => item.moderationInfo?.severity === 'sensitive');

export const DetectionLabels = ({ detections }) => {
  if (!detections?.length) return null;

  return (
    <dl className="mt-2 space-y-1 text-xs text-ide-muted">
      {detections.map((detection) => (
        <div key={detection.id} className="flex items-center justify-between gap-3">
          <dt className="capitalize text-ide-text">
            {detection.label.replace(/_/g, ' ').toLowerCase()}
          </dt>
          <dd>{Math.round((detection.confidence || 0) * 100)}%</dd>
        </div>
      ))}
    </dl>
  );
};

DetectionLabels.propTypes = {
  detections: PropTypes.arrayOf(
    PropTypes.shape({
      id: PropTypes.string.isRequired,
      label: PropTypes.string,
      confidence: PropTypes.number,
    })
  ),
};

export const InspectorOverlay = ({ boxes, metrics, naturalSize, onBoxClick }) => {
  if (!boxes?.length) return null;

  // Debug: 打印 metrics 和 naturalSize
  console.log('[InspectorOverlay] metrics:', metrics, 'naturalSize:', naturalSize);
  if (boxes.length > 0) {
    console.log('[InspectorOverlay] first box:', boxes[0]);
  }

  const boundsStyle = metrics
    ? {
        left: `${metrics.offsetX}px`,
        top: `${metrics.offsetY}px`,
        width: `${metrics.renderWidth}px`,
        height: `${metrics.renderHeight}px`,
      }
    : { left: 0, top: 0, width: '100%', height: '100%' };

  return (
    <div className="absolute inset-0 pointer-events-none">
      <div className="absolute" style={boundsStyle}>
        {boxes.map((box) => {
          let { x, y, width, height } = box.box;

          if (box.box.unit === 'pixel' && naturalSize?.width && naturalSize?.height) {
            x = x / naturalSize.width;
            y = y / naturalSize.height;
            width = width / naturalSize.width;
            height = height / naturalSize.height;
          }

          const left = metrics
            ? metrics.renderWidth * x
            : `${x * 100}%`;
          const top = metrics ? metrics.renderHeight * y : `${y * 100}%`;
          const widthVal = metrics
            ? metrics.renderWidth * width
            : `${width * 100}%`;
          const heightVal = metrics
            ? metrics.renderHeight * height
            : `${height * 100}%`;

          const isText = box.type === 'text';

          return (
            <div
              key={box.id}
              className={cn(
                'absolute overflow-hidden rounded transition-colors',
                isText 
                  ? 'border border-blue-400/30 hover:bg-blue-400/10 cursor-pointer pointer-events-auto hover:border-blue-400'
                  : cn(
                      'border',
                      box.isSensitive && !isMildLabel(box.label)
                        ? 'border-ide-error'
                        : 'border-amber-400/80'
                    )
              )}
              style={{
                left: metrics ? `${left}px` : left,
                top: metrics ? `${top}px` : top,
                width: metrics ? `${widthVal}px` : widthVal,
                height: metrics ? `${heightVal}px` : heightVal,
              }}
              onClick={(e) => {
                if (onBoxClick) {
                    onBoxClick(box);
                }
              }}
              title={isText ? box.label : undefined}
            >
              {!isText && (
                  <div
                    className="h-full w-full"
                    style={{
                      backdropFilter: 'blur(18px)',
                      WebkitBackdropFilter: 'blur(18px)',
                      backgroundColor: box.isSensitive && !isMildLabel(box.label)
                        ? 'rgba(12,15,28,0.45)'
                        : 'rgba(234,179,8,0.25)',
                    }}
                  />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
};

InspectorOverlay.propTypes = {
  boxes: PropTypes.arrayOf(
    PropTypes.shape({
      id: PropTypes.string.isRequired,
      label: PropTypes.string,
      type: PropTypes.string,
      box: PropTypes.shape({
        x: PropTypes.number,
        y: PropTypes.number,
        width: PropTypes.number,
        height: PropTypes.number,
        unit: PropTypes.string,
      }).isRequired,
      isSensitive: PropTypes.bool,
    })
  ),
  metrics: PropTypes.shape({
    renderWidth: PropTypes.number,
    renderHeight: PropTypes.number,
    offsetX: PropTypes.number,
    offsetY: PropTypes.number,
  }),
  naturalSize: PropTypes.shape({
    width: PropTypes.number,
    height: PropTypes.number,
  }),
  onBoxClick: PropTypes.func,
};

export const InspectorImage = ({ item, overlayBoxes, blurred, onBoxClick, maxHeight, className }) => {
  const containerRef = useRef(null);
  const imageRef = useRef(null);
  const [metrics, setMetrics] = useState(null);
  const [naturalSize, setNaturalSize] = useState(null);

  const updateMetrics = useCallback(() => {
    const containerEl = containerRef.current;
    const imageEl = imageRef.current;
    if (!containerEl || !imageEl) return;

    const containerRect = containerEl.getBoundingClientRect();
    const imageRect = imageEl.getBoundingClientRect();
    if (!imageRect.width || !imageRect.height) return;

    // 获取容器边框宽度
    const borderLeft = containerEl.clientLeft || 0;
    const borderTop = containerEl.clientTop || 0;

    // 获取图片的原始尺寸
    const naturalWidth = imageEl.naturalWidth;
    const naturalHeight = imageEl.naturalHeight;
    
    if (naturalWidth && naturalHeight) {
      setNaturalSize({
        width: naturalWidth,
        height: naturalHeight,
      });
    }
    
    // 对于 max-w-full max-h-full 的 img：
    // img 元素的大小就是图像按比例缩放后的渲染大小
    // 图像内容正好填满 img 元素（不需要 object-contain 内部留白计算）
    // 只需计算 img 元素相对于容器 padding box 的位置
    setMetrics({
      renderWidth: imageRect.width,
      renderHeight: imageRect.height,
      offsetX: imageRect.left - containerRect.left - borderLeft,
      offsetY: imageRect.top - containerRect.top - borderTop,
    });
  }, []);

  useEffect(() => {
    const containerEl = containerRef.current;
    if (!containerEl) return undefined;

    const observer = new ResizeObserver(() => updateMetrics());
    observer.observe(containerEl);
    return () => observer.disconnect();
  }, [updateMetrics]);

  if (!item?.imageUrl) {
    return (
      <div className="flex aspect-square w-full items-center justify-center rounded border border-ide-border bg-ide-panel text-ide-muted">
        <EyeOff className="h-6 w-6" />
        <span className="ml-2 text-sm">Image not available</span>
      </div>
    );
  }

  // If maxHeight is provided, use inline-flex layout for intrinsic sizing
  // Otherwise use w-full h-full for filling parent container
  const containerClass = maxHeight 
    ? "relative overflow-hidden rounded border border-ide-border bg-black inline-flex items-center justify-center"
    : "relative w-full h-full overflow-hidden rounded border border-ide-border bg-black flex items-center justify-center";

  return (
    <div
      ref={containerRef}
      className={cn(containerClass, className)}
      style={maxHeight ? { maxHeight } : undefined}
    >
      <img
        ref={imageRef}
        src={item.imageUrl}
        alt={item.prompt || 'Generated result'}
        className={cn('max-w-full max-h-full object-contain transition', blurred && 'blur-xl')}
        style={maxHeight ? { maxHeight } : undefined}
        onLoad={updateMetrics}
        loading="lazy"
      />
      <InspectorOverlay
        boxes={overlayBoxes}
        metrics={metrics}
        naturalSize={naturalSize}
        onBoxClick={onBoxClick}
      />
    </div>
  );
};

InspectorImage.propTypes = {
  item: PropTypes.shape({
    imageUrl: PropTypes.string,
    prompt: PropTypes.string,
  }),
  overlayBoxes: PropTypes.array,
  blurred: PropTypes.bool,
  onBoxClick: PropTypes.func,
  maxHeight: PropTypes.oneOfType([PropTypes.string, PropTypes.number]),
  className: PropTypes.string,
};

const ActionButton = ({ icon: Icon, label, onClick, disabled, variant }) => (
  <button
    type="button"
    onClick={onClick}
    disabled={disabled}
    className={cn(
      'ide-button flex items-center gap-2 text-xs transition',
      variant === 'danger' && 'border-ide-error text-ide-error hover:bg-ide-error/10',
      disabled && 'cursor-not-allowed opacity-60'
    )}
  >
    <Icon className="h-3.5 w-3.5" />
    {label}
  </button>
);

ActionButton.propTypes = {
  icon: PropTypes.elementType.isRequired,
  label: PropTypes.string.isRequired,
  onClick: PropTypes.func,
  disabled: PropTypes.bool,
  variant: PropTypes.oneOf(['danger']),
};

const EmptyState = ({ safeMode }) => (
  <div className="flex h-full items-center justify-center rounded border border-dashed border-ide-border text-center text-sm text-ide-muted">
    <div className="space-y-2 px-6">
      <p>No completed images yet.</p>
      {safeMode !== SAFE_MODE_LEVELS.close && (
        <p className="text-xs text-ide-muted/70">
          Safe Mode will blur new results until moderation finishes.
        </p>
      )}
    </div>
  </div>
);

EmptyState.propTypes = {
  safeMode: PropTypes.string,
};

export function Gallery({
  history,
  onSelectPrompt,
  onEditImage,
  onDelete,
  safeMode,
  selectiveAmbiguity,
  fetchModeration,
  isConnected = true,
}) {
  const decoratedHistory = useMemo(() => {
    if (!Array.isArray(history)) return [];
    return history.map((item) => {
      const rawModeration = item?.nsfw || item?.moderation || null;
      return {
        ...item,
        imageUrl: buildImageUrl(item?.image),
        thumbnailUrl: buildImageUrl(item?.image, { width: 300 }),
        moderationInfo: normalizeModeration(rawModeration),
        hasModeration: Boolean(rawModeration),
      };
    });
  }, [history]);

  const [sortBy, setSortBy] = useState('newest');
  const [filterNsfw, setFilterNsfw] = useState(['all']);
  const [isFilterOpen, setIsFilterOpen] = useState(false);
  const filterRef = useRef(null);

  useEffect(() => {
    const handleClickOutside = (event) => {
      if (filterRef.current && !filterRef.current.contains(event.target)) {
        setIsFilterOpen(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const toggleFilter = (value) => {
    setFilterNsfw((prev) => {
      if (value === 'all') {
        return ['all'];
      }

      let newFilters = prev.filter((f) => f !== 'all');
      if (newFilters.includes(value)) {
        newFilters = newFilters.filter((f) => f !== value);
      } else {
        newFilters.push(value);
      }

      if (newFilters.length === 0) return ['all'];
      return newFilters;
    });
  };

  const filteredHistory = useMemo(() => {
    let result = [...decoratedHistory];

    // Filter out failed tasks to prevent placeholders from sticking around
    result = result.filter(item => item.status !== 'failed');

    if (!filterNsfw.includes('all')) {
      result = result.filter((item) => {
        const severity =
          item.moderationInfo?.severity === 'none'
            ? 'safe'
            : item.moderationInfo?.severity;
        return filterNsfw.includes(severity);
      });
    }

    result.sort((a, b) => {
      const dateA = a.completedAt || a.createdAt || 0;
      const dateB = b.completedAt || b.createdAt || 0;
      return sortBy === 'newest' ? dateB - dateA : dateA - dateB;
    });

    return result;
  }, [decoratedHistory, sortBy, filterNsfw]);

  const groupedHistory = useGroupedHistory(filteredHistory);
  const [selectedPromptId, setSelectedPromptId] = useState(null);
  const [selectionMode, setSelectionMode] = useState('none'); // 'none', 'compare', 'batch'
  const [selectedItems, setSelectedItems] = useState([]);
  const [isCompareModalOpen, setIsCompareModalOpen] = useState(false);
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [loadingModerationId, setLoadingModerationId] = useState(null);
  const [batchProcessing, setBatchProcessing] = useState(false);

  useEffect(() => {
    if (!decoratedHistory.length) {
      setIsModalOpen(false);
      setSelectedPromptId(null);
      return;
    }

    if (!isModalOpen) {
      return;
    }

    setSelectedPromptId((current) => {
      if (current && decoratedHistory.some((item) => item.promptId === current)) {
        return current;
      }
      const fallback = findFirstSensitiveItem(decoratedHistory) || decoratedHistory[0];
      return fallback?.promptId || null;
    });
  }, [decoratedHistory, isModalOpen]);

  const selectedItem = useMemo(
    () => decoratedHistory.find((item) => item.promptId === selectedPromptId) || null,
    [decoratedHistory, selectedPromptId]
  );

  useEffect(() => {
    if (!isModalOpen) return;
    if (!fetchModeration || !selectedItem) return;
    if (selectedItem.status !== 'completed') return;
    if (selectedItem.hasModeration) return;

    let active = true;
    setLoadingModerationId(selectedItem.promptId);
    fetchModeration(selectedItem.promptId)
      .catch(() => {})
      .finally(() => {
        if (active) {
          setLoadingModerationId((current) =>
            current === selectedItem.promptId ? null : current
          );
        }
      });

    return () => {
      active = false;
    };
  }, [selectedItem, fetchModeration, isModalOpen]);

  const handleSelect = (item) => {
    if (selectionMode === 'compare') {
      setSelectedItems((prev) => {
        if (prev.includes(item.promptId)) {
          return prev.filter((id) => id !== item.promptId);
        }
        if (prev.length >= 2) {
          return [prev[1], item.promptId];
        }
        return [...prev, item.promptId];
      });
      return;
    }
    if (selectionMode === 'batch') {
      setSelectedItems((prev) => {
        if (prev.includes(item.promptId)) {
          return prev.filter((id) => id !== item.promptId);
        }
        return [...prev, item.promptId];
      });
      return;
    }
    setSelectedPromptId(item.promptId);
    setIsModalOpen(true);
  };

  const handleCloseModal = () => {
    setIsModalOpen(false);
    setShowDeleteConfirm(false);
    setSelectedPromptId(null);
    setLoadingModerationId(null);
  };

  const handleDeleteClick = () => {
    setShowDeleteConfirm(true);
  };

  const handleConfirmDelete = () => {
    if (onDelete && selectedPromptId) {
      onDelete(selectedPromptId);
      handleCloseModal();
    }
  };

  const handleRerunModeration = async (promptId) => {
    if (!fetchModeration) return;
    setLoadingModerationId(promptId);
    try {
      await fetchModeration(promptId, { refresh: true });
    } finally {
      setLoadingModerationId((current) => (current === promptId ? null : current));
    }
  };

  const handleBatchRunNudeNet = async () => {
    if (!fetchModeration || selectedItems.length === 0) return;
    setBatchProcessing(true);
    
    // Process sequentially to avoid overloading the server
    for (const promptId of selectedItems) {
      try {
        await fetchModeration(promptId, { refresh: true });
      } catch (e) {
        console.error(`Failed to run moderation for ${promptId}`, e);
      }
    }
    
    setBatchProcessing(false);
    setSelectionMode('none');
    setSelectedItems([]);
  };

  const handlePrev = useCallback(() => {
    const currentIndex = filteredHistory.findIndex((item) => item.promptId === selectedPromptId);
    if (currentIndex > 0) {
      setSelectedPromptId(filteredHistory[currentIndex - 1].promptId);
    }
  }, [filteredHistory, selectedPromptId]);

  const handleNext = useCallback(() => {
    const currentIndex = filteredHistory.findIndex((item) => item.promptId === selectedPromptId);
    if (currentIndex < filteredHistory.length - 1) {
      setSelectedPromptId(filteredHistory[currentIndex + 1].promptId);
    }
  }, [filteredHistory, selectedPromptId]);

  useEffect(() => {
    if (!isModalOpen) return;
    const handleKeyDown = (e) => {
      if (e.key === 'ArrowLeft') handlePrev();
      if (e.key === 'ArrowRight') handleNext();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isModalOpen, handlePrev, handleNext]);

  const blurConfig = useMemo(() => {
    if (!selectedItem) {
      return { shouldBlurThumbnail: false, shouldBlurInspector: false, inspectorBoxes: [] };
    }
    return buildBlurConfig({
      safeMode,
      selectiveAmbiguity,
      moderation: selectedItem.moderationInfo,
      hasModeration: selectedItem.hasModeration,
    });
  }, [selectedItem, safeMode, selectiveAmbiguity]);

  const severity = selectedItem?.moderationInfo?.severity || 'none';

  const detectionGroups = useMemo(
    () =>
      selectedItem
        ? groupDetections(selectedItem?.moderationInfo?.detections)
        : { mild: [], sensitive: [] },
    [selectedItem]
  );

  const isAwaitingModeration = Boolean(selectedItem && !selectedItem.hasModeration);

  if (!decoratedHistory.length) {
    return (
      <div className="relative h-full w-full">
        <EmptyState safeMode={safeMode} />
      </div>
    );
  }

  return (
    <div className="h-full w-full relative flex flex-col overflow-hidden">
      <div className="flex-1 overflow-y-auto p-4">
        {/* Toolbar */}
        <div className="flex flex-wrap items-center gap-2 mb-4 p-2 bg-ide-panel rounded border border-ide-border sticky top-0 z-10 shadow-sm shrink-0">
        <div className="flex items-center gap-2 flex-1 min-w-[120px]">
            <span className="text-[10px] font-bold text-ide-muted uppercase whitespace-nowrap">Sort</span>
            <select 
                value={sortBy} 
                onChange={(e) => setSortBy(e.target.value)}
                className="w-full bg-ide-bg border border-ide-border text-xs text-ide-text rounded p-1 focus:outline-none focus:border-ide-accent"
            >
                <option value="newest">Newest</option>
                <option value="oldest">Oldest</option>
            </select>
        </div>
        <div className="hidden sm:block h-4 w-px bg-ide-border" />
        <div className="flex items-center gap-2 flex-1 min-w-[120px]" ref={filterRef}>
            <span className="text-[10px] font-bold text-ide-muted uppercase whitespace-nowrap">Filter</span>
            <div className="relative w-full">
              <button
                type="button"
                onClick={() => setIsFilterOpen(!isFilterOpen)}
                className="w-full flex items-center justify-between bg-ide-bg border border-ide-border text-xs text-ide-text rounded p-1 focus:outline-none focus:border-ide-accent"
              >
                <span className="truncate">
                  {filterNsfw.includes('all')
                    ? 'All'
                    : filterNsfw
                        .map((f) => f.charAt(0).toUpperCase() + f.slice(1))
                        .join(', ')}
                </span>
                <ChevronDown className="h-3 w-3 opacity-50" />
              </button>

              {isFilterOpen && (
                <div className="absolute top-full left-0 mt-1 w-full bg-ide-panel border border-ide-border rounded shadow-lg z-20 py-1">
                  {['all', 'safe', 'mild', 'sensitive'].map((option) => (
                    <button
                      key={option}
                      type="button"
                      onClick={() => toggleFilter(option)}
                      className="w-full flex items-center px-2 py-1.5 text-xs hover:bg-ide-hover text-left"
                    >
                      <div
                        className={cn(
                          'w-3 h-3 border rounded mr-2 flex items-center justify-center',
                          filterNsfw.includes(option)
                            ? 'bg-ide-accent border-ide-accent'
                            : 'border-ide-muted'
                        )}
                      >
                        {filterNsfw.includes(option) && (
                          <Check className="h-2.5 w-2.5 text-white" />
                        )}
                      </div>
                      <span className="capitalize">{option}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
        </div>

        <div className="hidden sm:block h-4 w-px bg-ide-border" />
        <button
            onClick={() => {
                if (selectionMode === 'compare') {
                    setSelectionMode('none');
                    setSelectedItems([]);
                } else {
                    setSelectionMode('compare');
                    setSelectedItems([]);
                }
            }}
            className={cn(
                "flex items-center gap-2 px-2 py-1 rounded text-xs font-medium transition-colors",
                selectionMode === 'compare' ? "bg-ide-accent text-white" : "text-ide-muted hover:text-ide-text hover:bg-ide-hover"
            )}
        >
            <ArrowLeftRight className="w-3 h-3" />
            Compare
        </button>

        <button
            onClick={() => {
                if (selectionMode === 'batch') {
                    setSelectionMode('none');
                    setSelectedItems([]);
                } else {
                    setSelectionMode('batch');
                    setSelectedItems([]);
                }
            }}
            className={cn(
                "flex items-center gap-2 px-2 py-1 rounded text-xs font-medium transition-colors",
                selectionMode === 'batch' ? "bg-ide-accent text-white" : "text-ide-muted hover:text-ide-text hover:bg-ide-hover"
            )}
        >
            <ListChecks className="w-3 h-3" />
            Batch
        </button>
        
        {selectionMode === 'compare' && selectedItems.length === 2 && (
             <button
                onClick={() => setIsCompareModalOpen(true)}
                className="flex items-center gap-2 px-2 py-1 rounded text-xs font-medium bg-ide-accent text-white animate-in fade-in zoom-in duration-200"
            >
                Start Comparison
            </button>
        )}

        {selectionMode === 'batch' && selectedItems.length > 0 && (
            <>
             {selectedItems.length <= 4 && (
                <button
                    onClick={() => {
                        const items = selectedItems.map(id => decoratedHistory.find(i => i.promptId === id)).filter(Boolean);
                        if (items.length > 0 && onEditImage) {
                            const images = items.map(item => item.image);
                            onEditImage({ image: images }, true);
                            setSelectionMode('none');
                            setSelectedItems([]);
                        }
                    }}
                    className="flex items-center gap-2 px-2 py-1 rounded text-xs font-medium bg-ide-accent text-white animate-in fade-in zoom-in duration-200"
                >
                    <ListChecks className="w-3 h-3" />
                    Use in Edit ({selectedItems.length})
                </button>
             )}
             <button
                onClick={handleBatchRunNudeNet}
                disabled={batchProcessing}
                className="flex items-center gap-2 px-2 py-1 rounded text-xs font-medium bg-ide-accent text-white animate-in fade-in zoom-in duration-200 disabled:opacity-50 disabled:cursor-not-allowed"
            >
                {batchProcessing ? <Loader2 className="w-3 h-3 animate-spin" /> : <ShieldCheck className="w-3 h-3" />}
                Run NudeNet ({selectedItems.length})
            </button>
            </>
        )}
      </div>

      <div className="space-y-6 flex-1">
        {groupedHistory.map(({ id, dateLabel, items }) => (
          <section key={id}>
            <div className="mb-2 flex items-center justify-between gap-4">
              <h3 className="text-[10px] font-bold uppercase tracking-wide text-ide-muted">
                {dateLabel}
              </h3>
              <span className="hidden text-[10px] uppercase tracking-wide text-ide-muted/70 md:inline">
                点击缩略图查看详情
              </span>
            </div>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
              {items.map((item) => {
                const itemBlur = buildBlurConfig({
                  safeMode,
                  selectiveAmbiguity,
                  moderation: item.moderationInfo,
                  hasModeration: item.hasModeration,
                });
                const isActive = isModalOpen && item.promptId === selectedPromptId;
                const isSelected = selectedItems.includes(item.promptId);
                return (
                  <button
                    key={item.promptId}
                    type="button"
                    onClick={() => handleSelect(item)}
                    className={cn(
                      'group relative aspect-square overflow-hidden rounded border text-left transition',
                      'border-ide-border bg-ide-panel hover:border-ide-accent/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60',
                      isActive && 'border-ide-accent shadow-inner',
                      isSelected && 'border-ide-accent ring-2 ring-ide-accent/50'
                    )}
                  >
                    {isSelected && (
                        <div className="absolute top-2 right-2 z-10 bg-ide-accent text-white rounded-full p-0.5 shadow-md">
                            <Check className="w-3 h-3" />
                        </div>
                    )}
                    {item.imageUrl ? (
                      <img
                        src={item.thumbnailUrl || item.imageUrl}
                        alt={item.prompt || 'Generated thumbnail'}
                        className={cn(
                          'h-full w-full object-cover transition',
                          itemBlur.shouldBlurThumbnail && 'blur-md'
                        )}
                        loading="lazy"
                      />
                    ) : (
                      <div className="flex h-full w-full items-center justify-center bg-ide-bg text-ide-muted">
                        <EyeOff className="h-4 w-4" />
                      </div>
                    )}

                    <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-black/40 opacity-0 transition group-hover:opacity-100">
                      <Maximize2 className="h-4 w-4 text-white" />
                    </div>

                    {item.moderationInfo?.severity === 'sensitive' && (
                      <span className="absolute right-1 top-1 inline-flex items-center gap-1 rounded bg-ide-error px-1.5 py-0.5 text-[9px] font-semibold uppercase text-white">
                        <AlertTriangle className="h-3 w-3" />
                        Alert
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          </section>
        ))}
      </div>
      </div>


      {isModalOpen && selectedItem && (
      <Dialog
        isOpen={true}
        onClose={handleCloseModal}
        title={
          <div>
            <p className="text-[10px] font-semibold uppercase tracking-wide text-ide-muted">
              Image Details
            </p>
            <p className="text-[11px] text-ide-muted/80">
              #{selectedItem?.promptId?.slice(0, 8) || '—'} ·{' '}
              {new Date(selectedItem?.createdAt || Date.now()).toLocaleString()}
            </p>
          </div>
        }
        maxWidth="max-w-[960px]"
        contentClassName="flex flex-col gap-4 p-4 lg:flex-row"
      >
              <div className="group/nav relative flex-1 min-w-0">
                <button
                  type="button"
                  onClick={handlePrev}
                  disabled={
                    filteredHistory.findIndex((item) => item.promptId === selectedPromptId) <= 0
                  }
                  className="absolute left-2 top-1/2 z-10 -translate-y-1/2 rounded-full bg-black/50 p-2 text-white opacity-0 transition hover:bg-black/70 disabled:pointer-events-none disabled:opacity-0 group-hover/nav:opacity-100"
                >
                  <ChevronLeft className="h-6 w-6" />
                </button>

                <button
                  type="button"
                  onClick={handleNext}
                  disabled={
                    filteredHistory.findIndex((item) => item.promptId === selectedPromptId) >=
                    filteredHistory.length - 1
                  }
                  className="absolute right-2 top-1/2 z-10 -translate-y-1/2 rounded-full bg-black/50 p-2 text-white opacity-0 transition hover:bg-black/70 disabled:pointer-events-none disabled:opacity-0 group-hover/nav:opacity-100"
                >
                  <ChevronRight className="h-6 w-6" />
                </button>

                <InspectorImage
                  item={selectedItem}
                  overlayBoxes={selectiveAmbiguity ? blurConfig.inspectorBoxes : []}
                  blurred={
                    safeMode !== SAFE_MODE_LEVELS.close && blurConfig.shouldBlurInspector
                  }
                />
              </div>

              <div className="flex w-full flex-shrink-0 flex-col gap-4 lg:w-80">
                <div className="rounded border border-ide-border bg-ide-panel p-4">
                  <h2 className="text-sm font-semibold uppercase tracking-wide text-ide-muted">
                    Prompt
                  </h2>
                  <p className="mt-2 max-h-[180px] overflow-y-auto whitespace-pre-wrap text-sm text-ide-text">
                    {selectedItem.prompt || 'Unknown prompt'}
                  </p>
                </div>

                <div className="rounded border border-ide-border bg-ide-panel p-4">
                  <h2 className="text-sm font-semibold uppercase tracking-wide text-ide-muted">
                    Details
                  </h2>
                  <div className="mt-2 space-y-2 text-sm text-ide-text">
                    <div className="flex justify-between">
                      <span className="text-ide-muted">Seed</span>
                      <span className="font-mono">{selectedItem.seed ?? 'N/A'}</span>
                    </div>
                    {selectedItem.width && selectedItem.height && (
                      <div className="flex justify-between">
                        <span className="text-ide-muted">Resolution</span>
                        <span className="font-mono">
                          {selectedItem.width} x {selectedItem.height}
                        </span>
                      </div>
                    )}
                  </div>
                </div>

                <div className="rounded border border-ide-border bg-ide-panel p-4">
                  <div className="flex items-center justify-between">
                    <h3 className="text-sm font-semibold uppercase tracking-wide text-ide-muted">
                      Moderation
                    </h3>
                    <div
                      className={cn(
                        'inline-flex items-center gap-1 rounded px-2 py-0.5 text-[11px] font-semibold uppercase',
                        severity === 'sensitive' && 'bg-ide-error text-white',
                        severity === 'mild' && 'bg-amber-500/30 text-amber-200',
                        severity === 'none' && 'bg-emerald-500/20 text-emerald-200'
                      )}
                    >
                      {severity}
                    </div>
                  </div>

                  {isAwaitingModeration ? (
                    <div className="mt-4 flex items-center gap-2 text-sm text-ide-muted">
                      <Loader2 className="h-4 w-4 animate-spin" />
                      Awaiting NudeNet results…
                    </div>
                  ) : (
                    <>
                      {severity === 'none' && (
                        <p className="mt-3 text-sm text-ide-muted">
                          NudeNet did not flag any regions in this image.
                        </p>
                      )}

                      {severity !== 'none' && (
                        <div className="mt-3 space-y-4">
                          {detectionGroups.sensitive.length > 0 && (
                            <div>
                              <p className="flex items-center gap-1 text-xs font-semibold uppercase tracking-wide text-ide-error">
                                <AlertTriangle className="h-3 w-3" />
                                Sensitive
                              </p>
                              <DetectionLabels detections={detectionGroups.sensitive} />
                            </div>
                          )}

                          {detectionGroups.mild.length > 0 && (
                            <div>
                              <p className="text-xs font-semibold uppercase tracking-wide text-amber-200">
                                Mild
                              </p>
                              <DetectionLabels detections={detectionGroups.mild} />
                            </div>
                          )}
                        </div>
                      )}
                    </>
                  )}
                </div>

                <div className="rounded border border-ide-border bg-ide-panel p-4">
                  <h3 className="text-sm font-semibold uppercase tracking-wide text-ide-muted">
                    Actions
                  </h3>
                  <div className="mt-3 flex flex-wrap gap-2">
                    {onEditImage && selectedItem.imageUrl && (
                      <>
                        <ActionButton
                            icon={Edit}
                            label="Edit Image"
                            onClick={() => {
                            onEditImage(selectedItem);
                            handleCloseModal();
                            }}
                        />
                        <ActionButton
                            icon={ListChecks}
                            label="Use in edit"
                            onClick={() => {
                            onEditImage(selectedItem, true);
                            handleCloseModal();
                            }}
                        />
                      </>
                    )}
                    {selectedItem.imageUrl && (
                      <a
                        href={selectedItem.imageUrl}
                        target="_blank"
                        rel="noreferrer"
                        className="ide-button inline-flex items-center gap-2 text-xs"
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                        Open image
                      </a>
                    )}
                    {onDelete && (
                      <ActionButton
                        icon={Trash2}
                        label="Delete"
                        variant="danger"
                        onClick={handleDeleteClick}
                      />
                    )}
                  </div>
                  {safeMode === SAFE_MODE_LEVELS.close && (
                    <p className="mt-3 text-[11px] text-ide-muted/70">
                      Safe Mode is currently off. Re-enable it in settings to blur sensitive
                      detections automatically.
                    </p>
                  )}
                  {selectiveAmbiguity && safeMode !== SAFE_MODE_LEVELS.close && (
                    <p className="mt-2 text-[11px] text-ide-muted/70">
                      Selective ambiguity masks only the detected regions while reviewing.
                    </p>
                  )}
                </div>
              </div>
      </Dialog>
      )}

      {showDeleteConfirm && (
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        title="Confirm Deletion"
        message="Are you sure you want to delete this image? This action cannot be undone."
        confirmLabel="Delete"
        cancelLabel="Cancel"
        confirmVariant="danger"
        onCancel={() => setShowDeleteConfirm(false)}
        onConfirm={handleConfirmDelete}
      />
      )}

      {isCompareModalOpen && (
      <ImageCompareDialog
        isOpen={isCompareModalOpen}
        onClose={() => setIsCompareModalOpen(false)}
        image1={decoratedHistory.find(i => i.promptId === selectedItems[0])?.imageUrl}
        image2={decoratedHistory.find(i => i.promptId === selectedItems[1])?.imageUrl}
      />
      )}
    </div>
  );
}

Gallery.propTypes = {
  history: PropTypes.arrayOf(
    PropTypes.shape({
      promptId: PropTypes.string.isRequired,
      prompt: PropTypes.string,
      seed: PropTypes.number,
      width: PropTypes.number,
      height: PropTypes.number,
      createdAt: PropTypes.number,
      completedAt: PropTypes.number,
      status: PropTypes.string,
      image: PropTypes.shape({
        filename: PropTypes.string,
        subfolder: PropTypes.string,
        type: PropTypes.string,
      }),
      nsfw: PropTypes.object,
      moderation: PropTypes.object,
    })
  ),
  onSelectPrompt: PropTypes.func,
  onEditImage: PropTypes.func,
  onDelete: PropTypes.func,
  safeMode: PropTypes.string,
  selectiveAmbiguity: PropTypes.bool,
  fetchModeration: PropTypes.func,
};

Gallery.defaultProps = {
  history: [],
  onSelectPrompt: undefined,
  onEditImage: undefined,
  onDelete: undefined,
  safeMode: SAFE_MODE_LEVELS.close,
  selectiveAmbiguity: false,
  fetchModeration: undefined,
};

export default Gallery;
