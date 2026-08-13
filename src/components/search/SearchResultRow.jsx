import React, { useEffect, useMemo, useState } from 'react';
import { ChevronDown, Loader2, Maximize2, Eye } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { fetchThumbnail } from '../../lib/monitor_api';
import { CATEGORY_COLORS } from '../../lib/categories';
import { buildSnippet } from '../../lib/search_snippet';
import { captureDateOf } from '../../lib/search_grouping';

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** 把命中的关键词包成高亮片段。 */
function highlightMatches(text, tokens) {
  if (!text) return null;
  const usableTokens = tokens.filter(Boolean);
  if (usableTokens.length === 0) return text;

  const pattern = new RegExp(`(${usableTokens.map(escapeRegExp).join('|')})`, 'gi');
  const lowered = usableTokens.map((token) => token.toLowerCase());

  return text.split(pattern).map((segment, index) => (
    lowered.includes(segment.toLowerCase()) ? (
      <mark key={`${segment}-${index}`} className="rounded-sm bg-ide-accent/20 px-px font-semibold text-ide-accent">
        {segment}
      </mark>
    ) : (
      <React.Fragment key={`${segment}-${index}`}>{segment}</React.Fragment>
    )
  ));
}

/**
 * 结果行的时间显示：当天只给时分，跨天补上日期，跨年补上年份。
 * 完整时间戳留在 title 属性里。
 */
function formatHitTime(date) {
  if (!date) return { short: null, full: null };

  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  const sameYear = date.getFullYear() === now.getFullYear();

  const time = date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  if (sameDay) return { short: time, full: date.toLocaleString() };

  const day = date.toLocaleDateString(undefined, sameYear
    ? { month: 'numeric', day: 'numeric' }
    : { year: 'numeric', month: 'numeric', day: 'numeric' });
  return { short: `${day} ${time}`, full: date.toLocaleString() };
}

/** 结果缩略图，优先用批量预取的缓存，缺失时才单独补拉。 */
export function HitThumbnail({ item, preloadedSrc, className = 'h-[75px] w-[120px]' }) {
  const { t } = useTranslation();
  const [src, setSrc] = useState(preloadedSrc);
  const [loading, setLoading] = useState(!preloadedSrc);

  useEffect(() => {
    if (preloadedSrc) {
      setSrc(preloadedSrc);
      setLoading(false);
      return undefined;
    }

    let active = true;
    (async () => {
      const screenshotId = item.screenshot_id ?? item.metadata?.screenshot_id;
      const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
      const path = item.image_path || item.metadata?.image_path || item.path;
      if (!id && !path) {
        setLoading(false);
        return;
      }
      setLoading(true);
      const dataUrl = await fetchThumbnail(id, id ? null : path);
      if (active) {
        setSrc(dataUrl);
        setLoading(false);
      }
    })();
    return () => { active = false; };
  }, [item, preloadedSrc]);

  return (
    <div className={`flex items-center justify-center overflow-hidden rounded border border-ide-border bg-black text-[10px] text-ide-muted ${className}`}>
      {loading && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
      {!loading && src && <img src={src} alt="" className="h-full w-full object-cover" />}
      {!loading && !src && <span>{t('advancedSearch.no_image')}</span>}
    </div>
  );
}

/**
 * 一组搜索结果。
 *
 * 相邻时间里同一窗口的重复命中会被合并成一组，主条目展示最新的一条，
 * 其余的收进「同一窗口另有 N 条」，展开后按时间列出。
 *
 * @param {object} props
 * @param {{ primary: object, duplicates: object[] }} props.group 结果分组
 * @param {'ocr'|'nl'} props.mode 当前检索模式
 * @param {string[]} props.queryTokens 查询关键词，用于高亮和摘要开窗
 * @param {Record<number, string>} props.thumbnailCache 批量预取的缩略图
 * @param {(item: object) => void} props.onSelect 在主预览区打开
 * @param {(item: object) => void} [props.onOpenFloatingPreview] 在独立窗口打开
 */
export function SearchResultRow({ group, mode, queryTokens, thumbnailCache, onSelect, onOpenFloatingPreview }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const { primary, duplicates } = group;

  const processName = mode === 'nl' ? primary.metadata?.process_name : primary.process_name;
  const windowTitle = mode === 'nl' ? primary.metadata?.window_title : primary.window_title;
  const category = primary.category || primary.metadata?.category || null;
  const rawText = mode === 'nl' ? primary.ocr_text : primary.text;

  const snippet = useMemo(() => buildSnippet(rawText, queryTokens), [rawText, queryTokens]);
  const time = useMemo(() => formatHitTime(captureDateOf(primary)), [primary]);

  const normalize = (item) => ({
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.metadata?.image_path || item.path,
  });

  const cardClickBehavior = localStorage.getItem('cardClickBehavior_search') || 'preview';
  const isStandaloneDefault = cardClickBehavior === 'standalone' && !!onOpenFloatingPreview;

  const handleSelect = (item) => {
    const payload = normalize(item);
    if (isStandaloneDefault) onOpenFloatingPreview(payload);
    else onSelect?.(payload);
  };

  const handleAlternate = (event, item) => {
    event.preventDefault();
    event.stopPropagation();
    const payload = normalize(item);
    if (isStandaloneDefault) onSelect?.(payload);
    else onOpenFloatingPreview?.(payload);
  };

  const thumbnailFor = (item) => thumbnailCache[item.screenshot_id ?? item.metadata?.screenshot_id] || null;

  return (
    <div className="group rounded-lg px-2.5 py-2.5 transition-colors hover:bg-ide-hover">
      <div className="flex gap-3.5">
        <div className="relative shrink-0">
          <button
            type="button"
            onClick={() => handleSelect(primary)}
            className="block rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70"
          >
            <HitThumbnail item={primary} preloadedSrc={thumbnailFor(primary)} />
          </button>
          {onOpenFloatingPreview && (
            <button
              type="button"
              onClick={(event) => handleAlternate(event, primary)}
              title={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
              aria-label={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
              className="absolute right-1 top-1 rounded border border-ide-border bg-ide-panel/95 p-1 text-ide-muted opacity-0 shadow-sm transition hover:bg-ide-hover hover:text-ide-text focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70 group-hover:opacity-100"
            >
              {isStandaloneDefault ? <Eye className="h-3 w-3" /> : <Maximize2 className="h-3 w-3" />}
            </button>
          )}
        </div>

        <div className="flex min-w-0 flex-1 flex-col justify-center">
          <button
            type="button"
            onClick={() => handleSelect(primary)}
            className="block w-full rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70"
          >
            <div className="flex items-baseline gap-2 text-xs text-ide-muted">
              <span className="shrink-0 font-mono font-semibold text-ide-text">
                {processName || t('advancedSearch.unknown')}
              </span>
              {windowTitle && (
                <>
                  <span className="shrink-0 text-ide-border" aria-hidden="true">·</span>
                  <span className="truncate" title={windowTitle}>{windowTitle}</span>
                </>
              )}
              {time.short && (
                <span className="ml-auto shrink-0 pl-2.5 font-mono text-[11px] tabular-nums" title={time.full}>
                  {time.short}
                </span>
              )}
            </div>

            <p className="mt-1 line-clamp-2 text-[13px] leading-relaxed text-ide-text">
              {snippet
                ? highlightMatches(snippet, queryTokens)
                : <span className="italic text-ide-muted">{t('advancedSearch.no_ocr_text')}</span>}
            </p>
          </button>

          <div className="mt-1.5 flex items-center gap-2.5">
            {category && (
              <span className="flex items-center gap-1.5 text-[11px] text-ide-muted">
                <span
                  className="h-1.5 w-1.5 rounded-full"
                  style={{ backgroundColor: CATEGORY_COLORS[category] || '#6b7280' }}
                />
                {category}
              </span>
            )}
            {mode === 'nl' && primary.similarity !== undefined && (
              <span className="font-mono text-[11px] tabular-nums text-ide-muted">
                {t('advancedSearch.similarity', { score: primary.similarity.toFixed(2) })}
              </span>
            )}
            {duplicates.length > 0 && (
              <button
                type="button"
                onClick={() => setExpanded((prev) => !prev)}
                aria-expanded={expanded}
                className="ml-auto flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] text-ide-muted transition-colors hover:bg-ide-active hover:text-ide-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70"
              >
                {expanded
                  ? t('advancedSearch.group.collapse')
                  : t('advancedSearch.group.expand', { count: duplicates.length })}
                <ChevronDown className={`h-3 w-3 transition-transform ${expanded ? 'rotate-180' : ''}`} />
              </button>
            )}
          </div>

          {expanded && duplicates.length > 0 && (
            <ul className="mt-1.5 flex flex-col border-l-2 border-ide-border pl-3">
              {duplicates.map((item, index) => {
                const itemTime = formatHitTime(captureDateOf(item));
                const itemText = buildSnippet(mode === 'nl' ? item.ocr_text : item.text, queryTokens, { radius: 30, maxLength: 90 });
                return (
                  <li key={`${item.screenshot_id || item.id || index}-${index}`}>
                    <button
                      type="button"
                      onClick={() => handleSelect(item)}
                      className="flex w-full items-baseline gap-2.5 rounded px-2 py-1 text-left text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70"
                    >
                      <time className="shrink-0 font-mono text-[11px] tabular-nums" title={itemTime.full}>
                        {itemTime.short}
                      </time>
                      <span className="truncate">{highlightMatches(itemText, queryTokens)}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
