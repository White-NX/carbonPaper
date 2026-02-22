import React, { useCallback, useEffect, useRef, useState } from 'react';
import PropTypes from 'prop-types';
import { EyeOff } from 'lucide-react';
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

  const updateMetrics = useCallback(() => {
    const containerEl = containerRef.current;
    const imageEl = imageRef.current;
    if (!containerEl || !imageEl) return;

    const containerRect = containerEl.getBoundingClientRect();
    const imageRect = imageEl.getBoundingClientRect();
    if (!imageRect.width || !imageRect.height) return;

    const borderLeft = containerEl.clientLeft || 0;
    const borderTop = containerEl.clientTop || 0;

    const naturalWidth = imageEl.naturalWidth;
    const naturalHeight = imageEl.naturalHeight;
    
    if (naturalWidth && naturalHeight) {
      setNaturalSize({
        width: naturalWidth,
        height: naturalHeight,
      });
    }
    
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

export default InspectorImage;
