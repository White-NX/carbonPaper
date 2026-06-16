import React, { useCallback, useEffect, useRef, useState } from 'react';
import PropTypes from 'prop-types';
import { EyeOff, ZoomIn, ZoomOut, Maximize } from 'lucide-react';
import { cn } from '../lib/utils';

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
                  : 'border border-amber-400/80'
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
                      backgroundColor: 'rgba(234,179,8,0.25)',
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

  const [transform, setTransform] = useState({ scale: 1, x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const dragStartRef = useRef({ x: 0, y: 0 });

  const updateMetrics = useCallback(() => {
    const containerEl = containerRef.current;
    const imageEl = imageRef.current;
    if (!containerEl || !imageEl) return;

    const renderWidth = imageEl.offsetWidth;
    const renderHeight = imageEl.offsetHeight;
    if (!renderWidth || !renderHeight) return;

    const naturalWidth = imageEl.naturalWidth;
    const naturalHeight = imageEl.naturalHeight;
    
    if (naturalWidth && naturalHeight) {
      setNaturalSize({
        width: naturalWidth,
        height: naturalHeight,
      });
    }
    
    setMetrics({
      renderWidth,
      renderHeight,
      offsetX: imageEl.offsetLeft,
      offsetY: imageEl.offsetTop,
    });
  }, []);

  useEffect(() => {
    const containerEl = containerRef.current;
    if (!containerEl) return undefined;

    const observer = new ResizeObserver(() => updateMetrics());
    observer.observe(containerEl);
    return () => observer.disconnect();
  }, [updateMetrics]);

  const constrainPan = useCallback((newX, newY, newScale) => {
    const containerEl = containerRef.current;
    if (!containerEl) return { x: newX, y: newY };
    const rect = containerEl.getBoundingClientRect();
    const maxBoundX = rect.width * 0.8;
    const maxBoundY = rect.height * 0.8;
    return {
      x: Math.min(Math.max(newX, -rect.width * newScale + rect.width - maxBoundX), maxBoundX),
      y: Math.min(Math.max(newY, -rect.height * newScale + rect.height - maxBoundY), maxBoundY)
    };
  }, []);

  const handleWheel = useCallback((e) => {
    e.preventDefault();
    setTransform(prev => {
      const scaleAdjust = e.deltaY * -0.0015;
      let newScale = prev.scale * (1 + scaleAdjust);
      newScale = Math.min(Math.max(0.2, newScale), 10);
      const containerEl = containerRef.current;
      if (!containerEl) return prev;
      const rect = containerEl.getBoundingClientRect();
      const mouseX = e.clientX - rect.left;
      const mouseY = e.clientY - rect.top;
      const newX = mouseX - (mouseX - prev.x) * (newScale / prev.scale);
      const newY = mouseY - (mouseY - prev.y) * (newScale / prev.scale);
      return { scale: newScale, ...constrainPan(newX, newY, newScale) };
    });
  }, [constrainPan]);

  const handlePointerDown = useCallback((e) => {
    if (e.button !== 0) return;
    if (e.target.closest('.cursor-pointer')) return;
    e.preventDefault();
    setIsDragging(true);
    dragStartRef.current = { x: e.clientX - transform.x, y: e.clientY - transform.y };
  }, [transform.x, transform.y]);

  const handlePointerMove = useCallback((e) => {
    if (!isDragging) return;
    setTransform(prev => {
      const newX = e.clientX - dragStartRef.current.x;
      const newY = e.clientY - dragStartRef.current.y;
      return { ...prev, ...constrainPan(newX, newY, prev.scale) };
    });
  }, [isDragging, constrainPan]);

  const handlePointerUp = useCallback(() => {
    setIsDragging(false);
  }, []);

  const resetZoom = useCallback(() => {
    setTransform({ scale: 1, x: 0, y: 0 });
  }, []);

  const zoomIn = useCallback(() => {
    setTransform(prev => {
      const newScale = Math.min(prev.scale * 1.5, 10);
      const containerEl = containerRef.current;
      const rect = containerEl ? containerEl.getBoundingClientRect() : { width: 0, height: 0 };
      const centerX = rect.width / 2;
      const centerY = rect.height / 2;
      const newX = centerX - (centerX - prev.x) * (newScale / prev.scale);
      const newY = centerY - (centerY - prev.y) * (newScale / prev.scale);
      return { scale: newScale, ...constrainPan(newX, newY, newScale) };
    });
  }, [constrainPan]);

  const zoomOut = useCallback(() => {
    setTransform(prev => {
      const newScale = Math.max(prev.scale / 1.5, 0.2);
      const containerEl = containerRef.current;
      const rect = containerEl ? containerEl.getBoundingClientRect() : { width: 0, height: 0 };
      const centerX = rect.width / 2;
      const centerY = rect.height / 2;
      const newX = centerX - (centerX - prev.x) * (newScale / prev.scale);
      const newY = centerY - (centerY - prev.y) * (newScale / prev.scale);
      return { scale: newScale, ...constrainPan(newX, newY, newScale) };
    });
  }, [constrainPan]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    el.addEventListener('wheel', handleWheel, { passive: false });
    return () => el.removeEventListener('wheel', handleWheel);
  }, [handleWheel]);

  if (!item?.imageUrl) {
    return (
      <div className="flex aspect-square w-full items-center justify-center rounded border border-ide-border bg-ide-panel text-ide-muted">
        <EyeOff className="h-6 w-6" />
        <span className="ml-2 text-sm">Image not available</span>
      </div>
    );
  }

  const containerClass = maxHeight 
    ? "relative overflow-hidden rounded border border-ide-border bg-black inline-flex items-center justify-center"
    : "relative w-full h-full overflow-hidden rounded border border-ide-border bg-black flex items-center justify-center";

  return (
    <div
      ref={containerRef}
      className={cn(containerClass, className, isDragging ? 'cursor-grabbing' : 'cursor-grab')}
      style={maxHeight ? { maxHeight } : undefined}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerLeave={handlePointerUp}
      onDoubleClick={resetZoom}
    >
      <div
        style={{
          transform: `translate(${transform.x}px, ${transform.y}px) scale(${transform.scale})`,
          transformOrigin: '0 0',
          transition: isDragging ? 'none' : 'transform 0.1s ease-out'
        }}
        className="w-full h-full flex items-center justify-center relative"
      >
        <img
          ref={imageRef}
          src={item.imageUrl}
          alt={item.prompt || 'Generated result'}
          className={cn('max-w-full max-h-full object-contain pointer-events-none transition', blurred && 'blur-xl')}
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

      {/* Zoom Controls */}
      <div
        className="absolute top-4 right-4 flex items-center gap-1 px-2 py-1.5 preview-action-bar border border-ide-border rounded-full z-20"
        onPointerDown={(event) => event.stopPropagation()}
      >
        <button
          onClick={zoomOut}
          className="p-1.5 hover:bg-ide-hover rounded-full text-ide-muted hover:text-ide-text transition-colors"
          title="Zoom Out"
        >
          <ZoomOut size={16} />
        </button>
        <button
          onClick={resetZoom}
          className="px-2 py-1 hover:bg-ide-hover rounded-full text-ide-muted hover:text-ide-text transition-colors font-mono text-[11px]"
          title="Reset Zoom"
        >
          {Math.round(transform.scale * 100)}%
        </button>
        <button
          onClick={zoomIn}
          className="p-1.5 hover:bg-ide-hover rounded-full text-ide-muted hover:text-ide-text transition-colors"
          title="Zoom In"
        >
          <ZoomIn size={16} />
        </button>
        <div className="w-px h-5 bg-ide-border/50 mx-0.5"></div>
        <button
          onClick={resetZoom}
          className="p-1.5 hover:bg-ide-hover rounded-full text-ide-muted hover:text-ide-text transition-colors"
          title="Fit to Window"
        >
          <Maximize size={16} />
        </button>
      </div>
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

export default InspectorImage;
