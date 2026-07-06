import { useCallback, useEffect, useMemo, useState } from 'react';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';

export function normalizeTimestampToMs(value, options = {}) {
  const { assumeUtc = false } = options;
  if (value === null || value === undefined || value === '') return null;

  if (typeof value === 'number' && !Number.isNaN(value)) {
    if (value > 1e12) return value;
    if (value > 1e10) return value;
    return value * 1000;
  }

  const raw = typeof value === 'string' ? value.trim() : String(value);
  if (!raw) return null;

  const numeric = Number(raw);
  if (!Number.isNaN(numeric)) {
    if (numeric > 1e12) return numeric;
    if (numeric > 1e10) return numeric;
    return numeric * 1000;
  }

  let iso = raw;
  if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(raw)) {
    iso = raw.replace(' ', 'T');
  }
  if (assumeUtc && !/[zZ]|[+\-]\d{2}:\d{2}$/.test(iso)) {
    iso = `${iso}Z`;
  }
  const parsed = new Date(iso);
  if (!Number.isNaN(parsed.getTime())) return parsed.getTime();

  return null;
}

export function useSelectedSnapshot() {
  const [selectedEvent, setSelectedEvent] = useState(null);
  const [selectedDetails, setSelectedDetails] = useState(null);
  const [selectedImageSrc, setSelectedImageSrc] = useState(null);
  const [isLoadingDetails, setIsLoadingDetails] = useState(false);
  const [lastError, setLastError] = useState(null);
  const [highlightedEventId, setHighlightedEventId] = useState(null);
  const [timelineJump, setTimelineJump] = useState(null);
  const [timelineRefreshKey, setTimelineRefreshKey] = useState(0);

  useEffect(() => {
    if (!selectedEvent) {
      setSelectedDetails(null);
      setSelectedImageSrc(null);
      setLastError(null);
      return;
    }

    setIsLoadingDetails(true);
    setLastError(null);
    setSelectedImageSrc(null);

    let cancelled = false;

    const loadData = async () => {
      try {
        const targetId = selectedEvent.id === -1 ? null : selectedEvent.id;
        const targetPath = selectedEvent.path || selectedEvent.image_path;


        const [det, img] = await Promise.all([
          getScreenshotDetails(targetId, targetPath),
          fetchImage(targetId, targetPath),
        ]);

        if (cancelled) return;


        if (det && det.error) {
          throw new Error(det.error);
        }
        setSelectedDetails(det);

        if (selectedEvent._fromNlSearch) {
          const recordCreatedAt = det?.record?.created_at;
          if (recordCreatedAt) {
            const dbTimestampMs = normalizeTimestampToMs(recordCreatedAt, { assumeUtc: true });
            if (dbTimestampMs && Math.abs((selectedEvent.timestamp || 0) - dbTimestampMs) > 5000) {
              setTimelineJump({ time: dbTimestampMs, ts: Date.now() });
            }
          }
        }


        if (!img) {
          console.warn('Image fetch returned null for ID:', selectedEvent.id);
        }
        setSelectedImageSrc(img);
        setIsLoadingDetails(false);
      } catch (err) {
        if (cancelled) return;
        console.error('Failed to load details', err);
        setLastError(err.message || 'Failed to load image details');
        setIsLoadingDetails(false);
      }
    };

    loadData();

    return () => {
      cancelled = true;
    };
  }, [selectedEvent]);

  const ocrBoxes = useMemo(() => {
    return (selectedDetails?.ocr_results || []).map((item, index) => {
      const points = item.box_coords || item.box;
      if (!points || !Array.isArray(points) || points.length === 0) {
        return null;
      }
      const xs = points.map((p) => p[0]);
      const ys = points.map((p) => p[1]);
      const minX = Math.min(...xs);
      const maxX = Math.max(...xs);
      const minY = Math.min(...ys);
      const maxY = Math.max(...ys);

      return {
        id: String(item.id ?? index),
        label: item.text,
        type: 'text',
        box: {
          x: minX,
          y: minY,
          width: maxX - minX,
          height: maxY - minY,
          unit: 'pixel',
        },
        isSensitive: false,
      };
    }).filter(Boolean);
  }, [selectedDetails]);

  const clearSelection = useCallback(() => {
    setSelectedEvent(null);
    setSelectedDetails(null);
    setSelectedImageSrc(null);
  }, []);

  const bumpTimelineRefresh = useCallback(() => {
    setTimelineRefreshKey((prev) => prev + 1);
  }, []);

  return {
    selectedEvent,
    setSelectedEvent,
    selectedDetails,
    selectedImageSrc,
    isLoadingDetails,
    lastError,
    highlightedEventId,
    setHighlightedEventId,
    timelineJump,
    setTimelineJump,
    timelineRefreshKey,
    ocrBoxes,
    clearSelection,
    bumpTimelineRefresh,
  };
}
