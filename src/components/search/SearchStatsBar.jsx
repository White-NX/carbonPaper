import React from 'react';
import { Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 结果统计行。
 *
 * 后端提供完整统计时显示总数；检索不完整或总数未知时显示已加载的条数。
 * 检索状态保留给辅助技术和悬停提示，正文只显示结果数和以秒计的真实耗时。
 *
 * @param {object} props
 * @param {number} props.count 已知总数或当前已加载的结果条数
 * @param {boolean} props.totalKnown 是否已知完整结果总数
 * @param {number | null} props.elapsedMs 本次检索耗时（毫秒）
 * @param {boolean} props.updating 是否还在补充相关回顾
 * @param {string | null} props.statusMessage 供辅助技术和悬停查看的检索状态
 */
export function SearchStatsBar({ count, totalKnown = false, elapsedMs, updating = false, statusMessage }) {
  const { t } = useTranslation();
  const results = t(totalKnown ? 'advancedSearch.stats.total' : 'advancedSearch.stats.loaded', { count });
  const label = Number.isFinite(elapsedMs)
    ? t('advancedSearch.stats.summary', { results, seconds: (Math.max(0, elapsedMs) / 1000).toFixed(2) })
    : results;

  return (
    <div className="search-stats flex shrink-0 items-center gap-2 px-6 pb-1 pt-3 text-[11px] text-ide-muted"
      role="status" title={statusMessage || undefined}>
      <span className="tabular-nums">{label}</span>
      {updating && <Loader2 className="h-3 w-3 shrink-0 animate-spin motion-reduce:animate-none" aria-hidden="true" />}
      {statusMessage && <span className="sr-only">{statusMessage}</span>}
    </div>
  );
}
