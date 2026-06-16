import React, { useState, useEffect } from 'react';
import { Loader2, Maximize2, Tag, Eye } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { fetchThumbnail } from '../lib/monitor_api';
import { CATEGORY_COLORS } from '../lib/categories';
import { cn } from '../lib/utils';

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

export function ThumbnailCard({
  item,
  onSelect,
  onOpenFloatingPreview,
  preloadedSrc = null,
  footerText = null,
  footerPersistent = false,
  selected = false,
  sourceType = 'preview',
}) {
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
  const footerPrimary = footerText || processName || t('advancedSearch.unknown');
  const overlayVisibilityClass = footerPersistent ? 'opacity-100' : 'opacity-0 group-hover:opacity-100';

  const normalizedItem = {
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.metadata?.image_path || item.path,
  };

  let cardClickBehavior = 'preview';
  if (sourceType === 'search') {
    cardClickBehavior = localStorage.getItem('cardClickBehavior_search') || 'preview';
  } else if (sourceType === 'tasks') {
    cardClickBehavior = localStorage.getItem('cardClickBehavior_tasks') || 'standalone';
  } else if (sourceType === 'clusters') {
    cardClickBehavior = localStorage.getItem('cardClickBehavior_clusters') || 'standalone';
  }
  const isStandaloneDefault = cardClickBehavior === 'standalone' && !!onOpenFloatingPreview;

  const handleSelect = (event) => {
    event.stopPropagation();
    if (isStandaloneDefault) {
      onOpenFloatingPreview(normalizedItem);
    } else {
      onSelect?.(normalizedItem);
    }
  };

  const handleAlternateAction = (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (isStandaloneDefault) {
      onSelect?.(normalizedItem);
    } else {
      onOpenFloatingPreview?.(normalizedItem);
    }
  };

  return (
    <div
      className={cn(
        'group relative block w-full aspect-video overflow-hidden rounded border bg-ide-panel transition focus-within:ring-2 focus-within:ring-ide-accent/60',
        selected
          ? 'border-ide-accent ring-1 ring-ide-accent/50'
          : 'border-ide-border hover:border-ide-accent/70'
      )}
    >
      <button
        type="button"
        className="block h-full w-full text-left focus-visible:outline-none"
        onClick={handleSelect}
      >
        {imageSrc ? (
          <img src={imageSrc} alt="" className="h-full w-full object-cover" loading="lazy" />
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
          <div className={cn('absolute top-1 pointer-events-none', onOpenFloatingPreview ? 'left-1' : 'right-1')}>
            <CategoryBadge category={categoryValue} />
          </div>
        )}
        <div className={`pointer-events-none absolute inset-0 flex flex-col justify-end bg-gradient-to-t from-black/60 to-transparent transition p-2 ${overlayVisibilityClass}`}>
          <span className="text-xs text-white font-semibold truncate">{footerPrimary}</span>
          {similarity !== undefined && !footerPersistent && (
              <span className="text-[10px] text-white/80">
              {t('advancedSearch.similarity', { score: similarity.toFixed(2) })}
            </span>
          )}
        </div>
      </button>
      {onOpenFloatingPreview && (
        <button
          type="button"
          className="absolute right-1 top-1 z-10 rounded border border-white/15 bg-black/55 p-1.5 text-white opacity-0 shadow-sm transition hover:bg-black/75 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70 group-hover:opacity-100"
          onClick={handleAlternateAction}
          title={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
          aria-label={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
        >
          {isStandaloneDefault ? <Eye className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />}
        </button>
      )}
    </div>
  );
}
