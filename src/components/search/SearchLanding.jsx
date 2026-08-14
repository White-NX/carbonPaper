import React from 'react';
import { Clock, Lightbulb, SlidersHorizontal, Image as ImageIcon, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { HitThumbnail } from './SearchResultRow';

/** 着陆区里的分区标题。 */
function BlockHeading({ title, hint, action }) {
  return (
    <div className="flex items-baseline gap-2.5">
      <h3 className="text-[11px] font-semibold tracking-wider text-ide-muted">{title}</h3>
      {hint && <span className="text-[11px] text-ide-muted opacity-70">{hint}</span>}
      {action && <div className="ml-auto">{action}</div>}
    </div>
  );
}

/** 着陆区里的药丸按钮。 */
function LandingPill({ onClick, children }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex h-[30px] items-center gap-1.5 rounded-full border border-ide-border bg-ide-panel px-3 text-xs text-ide-text transition-colors hover:border-ide-accent hover:bg-ide-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60"
    >
      {children}
    </button>
  );
}

/**
 * 还没开始搜索时的着陆区。
 *
 * @param {object} props
 * @param {{ query: string, mode: string }[]} props.recentQueries 最近搜索记录
 * @param {(entry: { query: string, mode: string }) => void} props.onUseQuery 重跑某次搜索
 * @param {() => void} props.onClearRecent 清空搜索记录
 * @param {{ process_name: string, count: number }[]} props.processes 进程列表（含截图数）
 * @param {(processName: string) => void} props.onUseProcess 按来源筛选
 * @param {object[]} props.latest 最近捕获的截图
 * @param {Record<number, string>} props.thumbnailCache 已预取的缩略图
 * @param {(item: object) => void} props.onSelectLatest 打开某张截图
 * @param {string} props.eggMessage 轮换的彩蛋文案
 */
export function SearchLanding({
  recentQueries,
  onUseQuery,
  onClearRecent,
  processes,
  onUseProcess,
  latest,
  thumbnailCache,
  onSelectLatest,
  eggMessage,
}) {
  const { t } = useTranslation();
  const topProcesses = processes.slice(0, 6);
  const totalCaptures = processes.reduce((sum, entry) => sum + (Number(entry.count) || 0), 0);

  return (
    <div className="flex flex-1 flex-col gap-7 overflow-y-auto px-6 py-6 custom-scrollbar">
      {recentQueries.length > 0 && (
        <section className="flex flex-col gap-2.5">
          <BlockHeading
            title={t('advancedSearch.landing.recent_queries')}
            hint={t('advancedSearch.landing.recent_queries_hint')}
            action={(
              <button
                type="button"
                onClick={onClearRecent}
                className="text-[11px] text-ide-muted transition-colors hover:text-ide-text"
              >
                {t('advancedSearch.landing.clear_recent')}
              </button>
            )}
          />
          <div className="flex flex-wrap gap-2">
            {recentQueries.map((entry) => (
              <LandingPill key={`${entry.mode}-${entry.query}`} onClick={() => onUseQuery(entry)}>
                {entry.mode === 'nl'
                  ? <ImageIcon className="h-3 w-3 text-ide-muted" />
                  : <Clock className="h-3 w-3 text-ide-muted" />}
                {entry.query}
              </LandingPill>
            ))}
          </div>
        </section>
      )}

      {topProcesses.length > 0 && (
        <section className="flex flex-col gap-2.5">
          <BlockHeading
            title={t('advancedSearch.landing.top_apps')}
            hint={t('advancedSearch.landing.top_apps_hint')}
          />
          <div className="flex flex-wrap gap-2">
            {topProcesses.map((entry) => (
              <LandingPill key={entry.process_name} onClick={() => onUseProcess(entry.process_name)}>
                <span className="font-mono">{entry.process_name}</span>
                <span className="font-mono text-[11px] tabular-nums text-ide-muted">{entry.count}</span>
              </LandingPill>
            ))}
          </div>
        </section>
      )}

      {latest.length > 0 && (
        <section className="flex flex-col gap-2.5">
          <BlockHeading
            title={t('advancedSearch.landing.latest')}
            hint={totalCaptures > 0 ? t('advancedSearch.landing.latest_hint', { count: totalCaptures }) : null}
          />
          <div className="grid grid-cols-3 gap-2.5 sm:grid-cols-4 lg:grid-cols-6">
            {latest.map((item, index) => (
              <button
                key={`${item.screenshot_id || item.id || index}-${index}`}
                type="button"
                onClick={() => onSelectLatest(item)}
                className="group flex flex-col gap-1.5 rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60"
              >
                <div className="overflow-hidden rounded transition-transform group-hover:-translate-y-0.5">
                  <HitThumbnail
                    item={item}
                    preloadedSrc={thumbnailCache[item.screenshot_id ?? item.metadata?.screenshot_id] || null}
                    className="aspect-video w-full group-hover:border-ide-accent"
                  />
                </div>
                <span className="truncate font-mono text-[10px] text-ide-muted">
                  {item.process_name || item.metadata?.process_name || ''}
                </span>
              </button>
            ))}
          </div>
        </section>
      )}

      <div className="mt-auto flex items-center gap-1.5 pt-4 text-[11px] text-ide-muted opacity-60">
        <Lightbulb className="h-3 w-3" />
        {t('advancedSearch.search.did_you_know', { msg: eggMessage })}
      </div>
    </div>
  );
}

/**
 * 搜过但没搜到时的状态。
 *
 * 不用居中的大图标，因为这时候用户需要的是下一步该做什么，
 * 所以左对齐给出原因和两个可以直接点的出口。
 */
export function SearchNoResults({ query, mode, hasFilters, onClearFilters, onSwitchToVisual }) {
  const { t } = useTranslation();

  return (
    <div className="flex max-w-xl flex-col gap-2 px-6 py-14">
      <h3 className="flex items-center gap-2 text-sm font-semibold text-ide-text">
        <SlidersHorizontal className="h-4 w-4 text-ide-muted" />
        {query
          ? t('advancedSearch.search.no_results_for', { query })
          : t('advancedSearch.search.no_results_filtered')}
      </h3>
      <p className="text-xs leading-relaxed text-ide-muted">{t('advancedSearch.search.no_results_hint')}</p>
      <div className="mt-2.5 flex flex-wrap gap-2">
        {hasFilters && (
          <button
            type="button"
            onClick={onClearFilters}
            className="flex h-8 items-center gap-1.5 rounded-md border border-ide-border bg-ide-panel px-3.5 text-xs text-ide-text transition-colors hover:border-ide-accent hover:bg-ide-hover"
          >
            <RefreshCw className="h-3.5 w-3.5" />
            {t('advancedSearch.search.clear_filters')}
          </button>
        )}
        {mode === 'ocr' && onSwitchToVisual && (
          <button
            type="button"
            onClick={onSwitchToVisual}
            className="flex h-8 items-center gap-1.5 rounded-md border border-ide-border bg-ide-panel px-3.5 text-xs text-ide-text transition-colors hover:border-ide-accent hover:bg-ide-hover"
          >
            <ImageIcon className="h-3.5 w-3.5" />
            {t('advancedSearch.search.try_visual')}
          </button>
        )}
      </div>
    </div>
  );
}
