import React from 'react';
import { Clock, Sparkles } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 结果统计行。
 *
 * 搜索走的是盲位图索引，最终匹配需要解密验证，而且分页时按 `offset + limit`
 * 提前截断以免解密整个候选集，所以拿不到精确总数。这里只报已经加载到的
 * 条数和真实耗时，还有更多时明说，不去猜一个总量。
 *
 * @param {object} props
 * @param {number} props.count 当前已加载的结果条数
 * @param {number | null} props.elapsedMs 本次检索耗时（毫秒）
 * @param {boolean} props.hasMore 是否还有下一页
 * @param {'ocr'|'nl'} props.mode 当前检索模式，决定排序说明
 */
export function SearchStatsBar({ count, elapsedMs, hasMore, mode }) {
  const { t } = useTranslation();

  const parts = [t('advancedSearch.stats.loaded', { count })];
  if (typeof elapsedMs === 'number') {
    parts.push(t('advancedSearch.stats.elapsed', { ms: Math.max(1, Math.round(elapsedMs)) }));
  }
  if (hasMore) {
    parts.push(t('advancedSearch.stats.has_more'));
  }

  const SortIcon = mode === 'nl' ? Sparkles : Clock;

  return (
    <div className="flex shrink-0 items-center justify-between gap-4 border-b border-ide-border bg-ide-panel px-6 py-2 text-[11px] text-ide-muted">
      <span className="tabular-nums">{parts.join(' · ')}</span>
      <span className="flex shrink-0 items-center gap-1.5">
        <SortIcon className="h-3 w-3" />
        {mode === 'nl' ? t('advancedSearch.stats.sort_similarity') : t('advancedSearch.stats.sort_time')}
      </span>
    </div>
  );
}
