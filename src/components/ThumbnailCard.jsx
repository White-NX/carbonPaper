import React, { useState, useEffect } from 'react';
import { Loader2, Tag } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { fetchThumbnail } from '../lib/monitor_api';
import { CATEGORY_COLORS } from '../lib/categories';

export function CategoryBadge({ category }) {
  if (!category) return null;
  const color = CATEGORY_COLORS[category] || '#6b7280';
  return (
    <span
      className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap"
      style={{ backgroundColor: color + '22', color, border: `1px solid ${color}44` }}
    >
      <Tag className="w-2.5 h-2.5" />
      {category}
    </span>
  );
}

export function ThumbnailCard({ item, onSelect, preloadedSrc = null }) {
  const { t } = useTranslation();
  const [imageSrc, setImageSrc] = useState(preloadedSrc);
  const [loadingImage, setLoadingImage] = useState(!preloadedSrc);

  useEffect(() => {
    if (preloadedSrc) {
      setImageSrc(preloadedSrc);
      setLoadingImage(false);
      return;
    }
    let active = true;
    const loadImage = async () => {
      if (!item) return;
      const screenshotId = item.screenshot_id ?? item.metadata?.screenshot_id;
      const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
      const targetPath = item.image_path || item.metadata?.image_path || item.path;
      if (!id && !targetPath) return;
      setLoadingImage(true);
      const dataUrl = await fetchThumbnail(id, id ? null : targetPath);
      if (active) {
        setImageSrc(dataUrl);
        setLoadingImage(false);
      }
    };
    loadImage();
    return () => { active = false; };
  }, [item, preloadedSrc]);

  const processName = item.process_name || item.metadata?.process_name;
  const similarity = item.similarity;
  const categoryValue = item.category || item.metadata?.category || null;

  const normalizedItem = {
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.metadata?.image_path || item.path,
  };

  return (
    <button
      className="group relative aspect-video overflow-hidden rounded border
                 border-ide-border bg-ide-panel hover:border-ide-accent/70
                 transition focus-visible:outline-none focus-visible:ring-2
                 focus-visible:ring-ide-accent/60"
      onClick={(event) => {
        event.stopPropagation();
        onSelect(normalizedItem);
      }}
    >
      {imageSrc ? (
        <img src={imageSrc} className="h-full w-full object-cover" loading="lazy" />
      ) : loadingImage ? (
        <div className="flex h-full w-full items-center justify-center">
          <Loader2 className="w-4 h-4 animate-spin text-ide-muted" />
        </div>
      ) : (
        <div className="flex h-full w-full items-center justify-center bg-ide-bg text-ide-muted text-xs">
          {t('advancedSearch.no_image')}
        </div>
      )}
      {categoryValue && (
        <div className="absolute top-1 right-1 pointer-events-none">
          <CategoryBadge category={categoryValue} />
        </div>
      )}
      <div className="pointer-events-none absolute inset-0 flex flex-col justify-end
                      bg-gradient-to-t from-black/60 to-transparent opacity-0
                      transition group-hover:opacity-100 p-2">
        <span className="text-xs text-white font-semibold truncate">{processName || t('advancedSearch.unknown')}</span>
        {similarity !== undefined && (
            <span className="text-[10px] text-white/80">
            {t('advancedSearch.similarity', { score: similarity.toFixed(2) })}
          </span>
        )}
      </div>
    </button>
  );
}
