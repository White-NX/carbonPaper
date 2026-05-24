import React, { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Search, Loader2, Sparkles, AlertCircle, Zap, Info,
  ThumbsUp, ThumbsDown, Save, RotateCcw, X,
} from 'lucide-react';
import { nlClusterQuery, getRerankerStatus } from '../lib/task_api';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';

const SAMPLE_QUERIES = [
  '关于神经网络训练的代码与文档',
  '对加利福尼亚地区山脉的研究',
  '处理财务报表的电子表格',
];

const MIN_POSITIVES_FOR_SAVE = 3;

function formatSimilarity(sim) {
  if (sim === null || sim === undefined || Number.isNaN(sim)) return '—';
  return `${(sim * 100).toFixed(1)}%`;
}

function formatScore(score) {
  if (score === null || score === undefined || Number.isNaN(score)) return '—';
  return score.toFixed(2);
}

/**
 * Compute the per-cluster reranker threshold from calibration examples.
 *
 *   threshold = min(positive_scores) * 0.85
 *   if any negative score is >= that threshold, raise it to
 *      max(negative_scores) * 1.05
 *
 * Returns null if there are no positive examples with known scores.
 */
function computeThreshold(positives, negatives) {
  const posScores = positives.map(p => p.rerank_score).filter(s => typeof s === 'number');
  const negScores = negatives.map(p => p.rerank_score).filter(s => typeof s === 'number');
  if (!posScores.length) return null;
  const base = Math.min(...posScores) * 0.85;
  if (!negScores.length) return base;
  const negCeiling = Math.max(...negScores) * 1.05;
  return Math.max(base, negCeiling);
}

/**
 * Derive a stable accent color from the anchor text via FNV-1a hash.
 */
function colorFromAnchor(text) {
  let hash = 2166136261 >>> 0;
  for (let i = 0; i < text.length; i++) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 16777619) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue}, 65%, 55%)`;
}

/**
 * NlClusterView — used in two modes:
 *   - 'explore' (default): demo page for the NL retrieval pipeline
 *   - 'calibrate': example-picking page invoked from SmartClustersView when
 *                  the user wants to create a new smart cluster
 *
 * In calibrate mode, each result card gets ✅/❌ buttons that mark it as a
 * positive or negative example. The user can also click the card body to
 * preview the snapshot (jumps to the preview tab); selection state is
 * preserved because this component stays mounted while hidden.
 *
 * Props:
 *   mode                 - 'explore' | 'calibrate'
 *   backendOnline        - whether the Python backend is reachable
 *   onSelectScreenshot   - (item) => void; called when user clicks a card body
 *   onSaveCalibration    - ({ anchorText, threshold, examples, dominantColor }) => Promise<void>
 *                          required in 'calibrate' mode
 *   onCancelCalibration  - () => void; called when user cancels (close calibration)
 *   initialQuery         - string; pre-fills the input (used when re-entering calibration)
 */
export default function NlClusterView({
  mode = 'explore',
  backendOnline,
  onSelectScreenshot,
  onSaveCalibration,
  onCancelCalibration,
  initialQuery = '',
}) {
  const { t } = useTranslation();
  const isCalibrate = mode === 'calibrate';

  const [query, setQuery] = useState(initialQuery);
  const [nResults, setNResults] = useState(isCalibrate ? 30 : 30);
  // In calibrate mode reranker is always on (we need rerank_score for threshold).
  const [enableRerank, setEnableRerank] = useState(isCalibrate);
  const [rerankVariant, setRerankVariant] = useState('uint8');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [results, setResults] = useState([]);
  const [reranked, setReranked] = useState(false);
  const [activeVariant, setActiveVariant] = useState(null);
  const [lastQuery, setLastQuery] = useState('');
  const [thumbnailCache, setThumbnailCache] = useState({});
  const [rerankerStatus, setRerankerStatus] = useState(null);
  const [saving, setSaving] = useState(false);

  // Calibration selection: Map<screenshot_id, 'positive' | 'negative'>
  // Stored as a plain object for JSON serialization; the Map semantics are
  // simulated via direct mutation.
  const [selection, setSelection] = useState({});
  // Cache of rerank_score per screenshot_id from the most recent query —
  // used to derive the threshold at save time.
  const [scoreById, setScoreById] = useState({});

  const mountedRef = useRef(true);
  const cacheKeysRef = useRef([]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Check reranker availability whenever backend status / settings change
  useEffect(() => {
    if (!backendOnline) { setRerankerStatus(null); return; }
    let active = true;
    getRerankerStatus()
      .then(s => {
        if (!active) return;
        setRerankerStatus(s);
        if (s.available_variants.length) {
          setRerankVariant(prev => s.available_variants.includes(prev) ? prev : s.available_variants[0]);
        }
      })
      .catch(() => { if (active) setRerankerStatus({ available: false, loaded: false, available_variants: [], model_path: '' }); });
    return () => { active = false; };
  }, [backendOnline]);

  const handleSubmit = useCallback(async (e) => {
    e?.preventDefault?.();
    const trimmed = query.trim();
    if (!trimmed || !backendOnline) return;

    setLoading(true);
    setError(null);
    setResults([]);
    // In calibrate mode, clear selection when starting a fresh query against
    // a different anchor — but preserve it if the same query is re-run.
    if (isCalibrate && trimmed !== lastQuery) {
      setSelection({});
    }
    try {
      const { results: out, reranked: didRerank, rerank_variant: usedVariant } =
        await nlClusterQuery(trimmed, nResults, enableRerank, rerankVariant);
      setResults(out);
      setReranked(didRerank);
      setActiveVariant(usedVariant);
      setLastQuery(trimmed);
      // Snapshot scores for threshold computation later.
      const scoreMap = {};
      for (const r of out) {
        if (r.rerank_score !== undefined) scoreMap[r.screenshot_id] = r.rerank_score;
      }
      setScoreById(prev => ({ ...prev, ...scoreMap }));
    } catch (err) {
      const msg = String(err?.message || err);
      setError(msg);
      console.error('nl_cluster_query failed:', err);
    } finally {
      setLoading(false);
    }
  }, [query, nResults, enableRerank, rerankVariant, backendOnline, isCalibrate, lastQuery]);

  useEffect(() => {
    if (!results.length) return; // Do not clear the cache on empty search results
    let active = true;
    const ids = [...new Set(results
      .map(r => r.screenshot_id)
      .filter(id => typeof id === 'number' && id > 0))];
    // Filter out IDs that are already in the cache keys
    const missingIds = ids.filter(id => !cacheKeysRef.current.includes(id));
    if (!missingIds.length) return;

    fetchThumbnailBatch(missingIds)
      .then(batch => {
        if (!active || !batch) return;
        setThumbnailCache(prev => {
          const next = { ...prev, ...batch };
          const newKeys = Object.keys(batch).map(Number);
          
          // Append new keys, avoiding duplicates
          let updatedKeys = [...cacheKeysRef.current, ...newKeys];
          updatedKeys = [...new Set(updatedKeys)];

          // Evict oldest if exceeding 500
          if (updatedKeys.length > 500) {
            const evictCount = updatedKeys.length - 500;
            const evicted = updatedKeys.slice(0, evictCount);
            updatedKeys = updatedKeys.slice(evictCount);
            for (const id of evicted) {
              delete next[id];
            }
          }
          cacheKeysRef.current = updatedKeys;
          return next;
        });
      })
      .catch(err => console.error('thumbnail batch failed:', err));
    return () => { active = false; };
  }, [results]);

  const rerankUnavailable = enableRerank && rerankerStatus && !rerankerStatus.available;
  const availableVariants = rerankerStatus?.available_variants || [];

  const variantLabel = (v) => ({
    fp16: 'fp16 (~1.1GB)',
    q4f16: 'q4f16 (~670MB)',
    int8: 'int8',
    uint8: 'uint8 (~570MB)',
    fp32: 'fp32',
  }[v] || v);

  // Calibrate-mode handlers
  const toggleMark = (screenshotId, kind) => {
    setSelection(prev => {
      const next = { ...prev };
      if (next[screenshotId] === kind) {
        delete next[screenshotId];
      } else {
        next[screenshotId] = kind;
      }
      return next;
    });
  };

  const selectionCounts = useMemo(() => {
    let pos = 0, neg = 0;
    for (const v of Object.values(selection)) {
      if (v === 'positive') pos++;
      else if (v === 'negative') neg++;
    }
    return { pos, neg };
  }, [selection]);

  const handleSave = async () => {
    if (!isCalibrate || !onSaveCalibration || saving) return;
    const positives = Object.entries(selection)
      .filter(([, v]) => v === 'positive')
      .map(([sid]) => ({ screenshot_id: Number(sid), is_positive: true, rerank_score: scoreById[Number(sid)] }));
    const negatives = Object.entries(selection)
      .filter(([, v]) => v === 'negative')
      .map(([sid]) => ({ screenshot_id: Number(sid), is_positive: false, rerank_score: scoreById[Number(sid)] }));

    if (positives.length < MIN_POSITIVES_FOR_SAVE) {
      setError(t('nlCluster.errorMinPositives', '需要至少 {{count}} 个正例才能保存', { count: MIN_POSITIVES_FOR_SAVE }));
      return;
    }

    const threshold = computeThreshold(positives, negatives);
    if (threshold === null || Number.isNaN(threshold)) {
      setError(t('nlCluster.errorCalculateThreshold', '无法从正例分数计算阈值——请重新检索后再标记'));
      return;
    }

    setSaving(true);
    try {
      await onSaveCalibration({
        anchor_text: lastQuery,
        threshold,
        dominant_color: colorFromAnchor(lastQuery),
        examples: [...positives, ...negatives],
      });
      if (mountedRef.current) {
        // Reset on success
        setSelection({});
        setResults([]);
        setQuery('');
        setLastQuery('');
      }
    } catch (err) {
      if (mountedRef.current) {
        setError(err?.message || String(err));
      }
    } finally {
      if (mountedRef.current) {
        setSaving(false);
      }
    }
  };

  const handleResetSelection = () => {
    setSelection({});
  };

  // Computed threshold preview (only meaningful in calibrate mode)
  const thresholdPreview = useMemo(() => {
    if (!isCalibrate) return null;
    const positives = Object.entries(selection)
      .filter(([, v]) => v === 'positive')
      .map(([sid]) => ({ screenshot_id: Number(sid), rerank_score: scoreById[Number(sid)] }));
    const negatives = Object.entries(selection)
      .filter(([, v]) => v === 'negative')
      .map(([sid]) => ({ screenshot_id: Number(sid), rerank_score: scoreById[Number(sid)] }));
    if (positives.length < MIN_POSITIVES_FOR_SAVE) return null;
    return computeThreshold(positives, negatives);
  }, [isCalibrate, selection, scoreById]);

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* Toolbar */}
      <div className="shrink-0 border-b border-ide-border bg-ide-panel px-4 py-3 space-y-2">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-ide-accent" />
          <h2 className="text-sm font-semibold text-ide-text">
            {isCalibrate ? t('nlCluster.createSmartCluster', '创建智能聚类') : t('nlCluster.experimentalTitle', '自然语言聚类（demo）')}
          </h2>
          <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">
            {isCalibrate ? t('nlCluster.badgeCalibration', 'calibration') : t('nlCluster.badgeExperimental', 'experimental')}
          </span>
          {isCalibrate && onCancelCalibration && (
            <button
              onClick={onCancelCalibration}
              disabled={saving}
              className="ml-auto flex items-center gap-1 px-2 py-0.5 text-[11px] text-ide-muted hover:text-ide-text hover:bg-ide-hover/40 rounded transition-colors disabled:opacity-40 disabled:pointer-events-none"
              title={t('nlCluster.cancelTooltip', '取消并返回')}
            >
              <X className="w-3 h-3" />
              {t('nlCluster.cancel', '取消')}
            </button>
          )}
        </div>

        <form onSubmit={handleSubmit} className="flex items-center gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-ide-muted pointer-events-none" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={isCalibrate
                ? t('nlCluster.calibratePlaceholder', '描述你想自动归档的内容…（如 "对加利福尼亚地区山脉的研究"）')
                : t('nlCluster.explorePlaceholder', '试试 "关于神经网络训练的代码与文档" …')}
              className="w-full pl-8 pr-3 py-1.5 text-xs bg-ide-bg border border-ide-border rounded-lg text-ide-text placeholder-ide-muted focus:outline-none focus:border-ide-accent"
              disabled={loading}
            />
          </div>
          <select
            value={nResults}
            onChange={(e) => setNResults(Number(e.target.value))}
            disabled={loading}
            className="px-2 py-1.5 text-xs bg-ide-bg border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
            title={t('nlCluster.resultsLimitTooltip', '返回结果数量')}
          >
            <option value={10}>{t('nlCluster.topLimit', 'top {{count}}', { count: 10 })}</option>
            <option value={30}>{t('nlCluster.topLimit', 'top {{count}}', { count: 30 })}</option>
            <option value={60}>{t('nlCluster.topLimit', 'top {{count}}', { count: 60 })}</option>
            <option value={120}>{t('nlCluster.topLimit', 'top {{count}}', { count: 120 })}</option>
          </select>
          <button
            type="submit"
            disabled={loading || !backendOnline || !query.trim()}
            className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-accent bg-ide-accent/20 text-ide-accent hover:bg-ide-accent/30 disabled:opacity-40 transition-colors"
          >
            {loading ? <Loader2 className="w-3 h-3 animate-spin" /> : <Search className="w-3 h-3" />}
            {isCalibrate ? t('nlCluster.previewCandidates', '预览候选') : t('nlCluster.search', '检索')}
          </button>
        </form>

        {/* Mode-specific second row */}
        {!isCalibrate ? (
          <div className="flex items-center gap-2 flex-wrap">
            <label className={`flex items-center gap-1.5 px-2 py-1 text-[11px] rounded border cursor-pointer transition-colors ${
              enableRerank
                ? 'bg-ide-accent/15 border-ide-accent/40 text-ide-accent'
                : 'bg-ide-bg border-ide-border text-ide-muted hover:bg-ide-hover/30'
            }`}>
              <input
                type="checkbox"
                checked={enableRerank}
                onChange={(e) => setEnableRerank(e.target.checked)}
                className="w-3 h-3 accent-ide-accent"
              />
              <Zap className="w-3 h-3" />
              {t('nlCluster.enableReranker', '启用 reranker')}
            </label>

            {enableRerank && availableVariants.length > 0 && (
              <select
                value={rerankVariant}
                onChange={(e) => setRerankVariant(e.target.value)}
                disabled={loading}
                className="px-2 py-1 text-[11px] bg-ide-bg border border-ide-border rounded text-ide-text focus:outline-none focus:border-ide-accent"
                title={t('nlCluster.onnxVariantTooltip', 'ONNX 变体（切换会触发模型重新加载）')}
              >
                {availableVariants.map((v) => (
                  <option key={v} value={v}>{variantLabel(v)}</option>
                ))}
              </select>
            )}

            {rerankerStatus?.loaded && rerankerStatus.loaded_variant && (
              <span className="text-[10.5px] text-ide-muted">
                {t('nlCluster.currentlyLoaded', '当前已加载: ')}<span className="text-ide-text">{rerankerStatus.loaded_variant}</span>
                {rerankerStatus.provider && <span className="opacity-70"> · {rerankerStatus.provider.replace('ExecutionProvider', '')}</span>}
              </span>
            )}

            {rerankUnavailable && (
              <span className="flex items-center gap-1 text-[10.5px] text-amber-400">
                <Info className="w-3 h-3" />
                {t('nlCluster.modelNotFoundMsg', '未检测到 bge-reranker-v2-m3 模型（{{path}}）', { path: rerankerStatus.model_path })}
              </span>
            )}

            <span className="text-[11px] text-ide-muted ml-auto">{t('nlCluster.samplePrefix', '示例：')}</span>
            {SAMPLE_QUERIES.map((q) => (
              <button
                key={q}
                onClick={() => setQuery(q)}
                disabled={loading}
                className="px-2 py-0.5 text-[11px] rounded border border-ide-border text-ide-muted hover:text-ide-text hover:bg-ide-hover/40 transition-colors"
              >
                {q}
              </button>
            ))}
          </div>
        ) : (
          <div className="flex items-center gap-3 text-[11px] flex-wrap">
            <span className="inline-flex items-center gap-1 text-emerald-400">
              <ThumbsUp className="w-3 h-3" />
              {t('nlCluster.markedPositives', '已标记正例: ')}<span className="font-mono">{selectionCounts.pos}</span>
            </span>
            <span className="inline-flex items-center gap-1 text-rose-400">
              <ThumbsDown className="w-3 h-3" />
              {t('nlCluster.markedNegatives', '已标记反例: ')}<span className="font-mono">{selectionCounts.neg}</span>
            </span>
            {selectionCounts.pos > 0 && (
              <button
                onClick={handleResetSelection}
                className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-ide-muted hover:text-ide-text hover:bg-ide-hover/40 transition-colors"
                title={t('nlCluster.clearMarksTooltip', '清除所有标记')}
              >
                <RotateCcw className="w-3 h-3" />
                {t('nlCluster.clearMarks', '清除标记')}
              </button>
            )}
            {thresholdPreview !== null && (
              <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-ide-accent/10 text-ide-accent border border-ide-accent/30 ml-auto">
                <Info className="w-3 h-3" />
                {t('nlCluster.predictedThreshold', '预计逆值: ')}<span className="font-mono">{thresholdPreview.toFixed(2)}</span>
              </span>
            )}
            <span className="text-ide-muted/70 text-[10.5px] basis-full">
              {t('nlCluster.calibrationTip1', '提示：点击卡片可跳转到预览查看内容；返回此页时标记不会丢失。')}
              {t('nlCluster.calibrationTip2', ' 至少需要 {{count}} 个正例才能保存。', { count: MIN_POSITIVES_FOR_SAVE })}
            </span>
          </div>
        )}

        {error && (
          <div className="flex items-center gap-2 px-2.5 py-1.5 bg-red-500/10 border border-red-500/30 rounded-lg">
            <AlertCircle className="w-3.5 h-3.5 text-red-400 shrink-0" />
            <span className="text-xs text-red-400 break-all flex-1">{error}</span>
            <button onClick={() => setError(null)} className="text-red-400 hover:text-red-300">
              <X className="w-3 h-3" />
            </button>
          </div>
        )}
      </div>

      {/* Results */}
      <div className="flex-1 overflow-y-auto p-4">
        {loading ? (
          <div className="flex flex-col items-center justify-center h-40 gap-2 text-ide-muted">
            <Loader2 className="w-5 h-5 animate-spin" />
            <span className="text-xs">
              {enableRerank ? t('nlCluster.loadingReranking', '编码 → 召回 → 加载 reranker → 重排…') : t('nlCluster.loadingSearching', '正在编码查询并匹配快照…')}
            </span>
          </div>
        ) : !results.length ? (
          <div className="flex flex-col items-center justify-center h-40 gap-2 text-ide-muted text-sm">
            <Sparkles className="w-6 h-6 opacity-40" />
            <span>
              {lastQuery
                ? t('nlCluster.noResults', '没有匹配的快照')
                : isCalibrate
                  ? t('nlCluster.calibrateInstruction', '输入描述并点击"预览候选"，系统会列出最相关的快照供你标记')
                  : t('nlCluster.exploreInstruction', '输入一个自然语言描述，系统会从 hot 层向量索引中召回最相似的快照')}
            </span>
            <span className="text-[11px] opacity-70">
              {t('nlCluster.demoWarning', '本演示直接复用任务聚类的 MiniLM 向量库（仅限近 30 天 hot 层数据）')}
            </span>
          </div>
        ) : (
          <>
            <div className="flex items-center justify-between mb-3 text-[11px] text-ide-muted">
              <span>
                {t('nlCluster.queryPrefix', '查询：')}<span className="text-ide-text font-medium">{lastQuery}</span>
                {reranked && (
                  <span className="ml-2 inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-ide-accent/15 text-ide-accent">
                    <Zap className="w-2.5 h-2.5" />
                    reranked{activeVariant ? ` · ${activeVariant}` : ''}
                  </span>
                )}
              </span>
              <span>{t('nlCluster.resultsCount', '{{count}} 个结果', { count: results.length })}</span>
            </div>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
              {results.map((r) => {
                const footer = reranked && r.rerank_score !== undefined
                  ? t('nlCluster.scoreDetail', 'rerank {{score}} · 相似度 {{sim}}', { score: formatScore(r.rerank_score), sim: formatSimilarity(r.similarity) })
                  : t('nlCluster.similarityValue', '相似度 {{sim}}', { sim: formatSimilarity(r.similarity) });
                const mark = selection[r.screenshot_id];
                return (
                  <div key={r.screenshot_id} className="relative group">
                    {isCalibrate && (
                      <div className="absolute top-1 left-1 z-10 flex items-center gap-1">
                        <button
                          onClick={(e) => { e.stopPropagation(); toggleMark(r.screenshot_id, 'positive'); }}
                          className={`p-1 rounded transition-all ${
                            mark === 'positive'
                              ? 'bg-emerald-500 text-white shadow-md'
                              : 'bg-black/50 text-white/70 hover:bg-emerald-500/70 opacity-0 group-hover:opacity-100'
                          }`}
                          title={t('nlCluster.markPosTooltip', '标记为正例')}
                        >
                          <ThumbsUp className="w-3 h-3" />
                        </button>
                        <button
                          onClick={(e) => { e.stopPropagation(); toggleMark(r.screenshot_id, 'negative'); }}
                          className={`p-1 rounded transition-all ${
                            mark === 'negative'
                              ? 'bg-rose-500 text-white shadow-md'
                              : 'bg-black/50 text-white/70 hover:bg-rose-500/70 opacity-0 group-hover:opacity-100'
                          }`}
                          title={t('nlCluster.markNegTooltip', '标记为反例')}
                        >
                          <ThumbsDown className="w-3 h-3" />
                        </button>
                      </div>
                    )}
                    {/* Selection ring overlay */}
                    {mark && (
                      <div
                        className={`absolute inset-0 pointer-events-none rounded border-2 z-[5] ${
                          mark === 'positive' ? 'border-emerald-500' : 'border-rose-500'
                        }`}
                        aria-hidden="true"
                      />
                    )}
                    <ThumbnailCard
                      item={{
                        screenshot_id: r.screenshot_id,
                        process_name: r.process_name,
                        window_title: r.window_title,
                        category: r.category,
                        created_at: r.timestamp ? new Date(r.timestamp * 1000).toISOString() : null,
                      }}
                      preloadedSrc={thumbnailCache[r.screenshot_id] || null}
                      onSelect={(payload) => onSelectScreenshot?.(payload)}
                      footerText={footer}
                      footerPersistent
                    />
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>

      {/* Sticky action bar for calibrate mode */}
      {isCalibrate && (
        <div className="shrink-0 border-t border-ide-border bg-ide-panel px-4 py-2.5 flex items-center justify-end gap-2">
          {onCancelCalibration && (
            <button
              onClick={onCancelCalibration}
              disabled={saving}
              className="px-3 py-1.5 text-xs text-ide-muted hover:text-ide-text border border-ide-border rounded transition-colors disabled:opacity-50"
            >
              {t('nlCluster.cancel', '取消')}
            </button>
          )}
          <button
            onClick={handleSave}
            disabled={saving || selectionCounts.pos < MIN_POSITIVES_FOR_SAVE || !lastQuery}
            className="flex items-center gap-1.5 px-4 py-1.5 text-xs rounded bg-ide-accent text-white hover:bg-ide-accent/90 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
          >
            {saving ? <Loader2 className="w-3 h-3 animate-spin" /> : <Save className="w-3 h-3" />}
            {t('nlCluster.saveButton', '保存为智能聚类')}
            {selectionCounts.pos < MIN_POSITIVES_FOR_SAVE && (
              <span className="opacity-70 ml-1">({t('nlCluster.needMorePositives', '还需 {{count}} 个正例', { count: MIN_POSITIVES_FOR_SAVE - selectionCounts.pos })})</span>
            )}
          </button>
        </div>
      )}
    </div>
  );
}
