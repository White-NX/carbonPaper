import React, { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Search, Loader2, Sparkles, AlertCircle, Zap, Info,
  ThumbsUp, ThumbsDown, Save, RotateCcw, X, Square,
} from 'lucide-react';
import { nlClusterQuery, nlRerankStopNow, getRerankerStatus } from '../lib/task_api';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';
import { useTauriEventListener } from '../hooks/useTauriEventListener';

const SAMPLE_QUERIES = [
  '关于神经网络训练的代码与文档',
  '对加利福尼亚地区山脉的研究',
  '处理财务报表的电子表格',
];

const MIN_POSITIVES_FOR_SAVE = 3;

/**
 * Result counts the picker offers.
 *
 * Was 10 / 30 / 60 / 120. A reranked query costs a CPU cross-encoder pass per
 * candidate and pulls four candidates per requested result, so 120 asked for
 * 480 of them — which at the measured per-document latency could not finish
 * inside any budget worth waiting for, on any machine, including the one the
 * latency was measured on. The backend clamps to 30 for reranked queries
 * (`rerank.rs::MAX_RERANK_RESULTS`); this is the same bound where the user can
 * see it, so the picker does not offer a choice that would be silently ignored.
 */
const RESULT_LIMITS = [10, 30];

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
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [results, setResults] = useState([]);
  const [reranked, setReranked] = useState(false);
  const [activeVariant, setActiveVariant] = useState(null);
  const [lastQuery, setLastQuery] = useState('');
  // Which engine produced the scores currently in `scoreById`. Saved alongside
  // the threshold derived from them, because a reranked query is served by Rust
  // or by Python depending on conditions this screen cannot see, and the two
  // disagree enough that a threshold from one cannot be applied to the other.
  const [lastBackend, setLastBackend] = useState(null);
  const [thumbnailCache, setThumbnailCache] = useState({});
  const [rerankerStatus, setRerankerStatus] = useState(null);
  const [saving, setSaving] = useState(false);
  // Where the running query is, as the backend reports it after each chunk.
  // `null` between queries. Only reranked queries emit this; a plain retrieval
  // finishes in milliseconds and has nothing to report.
  const [progress, setProgress] = useState(null);
  const [stopping, setStopping] = useState(false);
  // Set when the last query ended because the user stopped it, so the empty
  // result area can say so instead of reading as "no matching snapshots".
  const [cancelled, setCancelled] = useState(false);

  // Calibration selection: Map<screenshot_id, 'positive' | 'negative'>
  // Stored as a plain object for JSON serialization; the Map semantics are
  // simulated via direct mutation.
  const [selection, setSelection] = useState({});
  // Cache of rerank_score per screenshot_id from the most recent query —
  // used to derive the threshold at save time.
  const [scoreById, setScoreById] = useState({});

  const mountedRef = useRef(true);
  const cacheKeysRef = useRef([]);
  // Whether a query of ours is still running in the backend. A ref because the
  // unmount cleanup below cannot read the latest `loading`.
  const inFlightRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      // Unmounting closes the view, not the query it started: without this the
      // backend keeps scoring for minutes with nobody left to watch or stop it,
      // and the next query has to share the semantic worker with it.
      if (inFlightRef.current) nlRerankStopNow().catch(() => {});
    };
  }, []);

  // Check reranker availability whenever backend status / settings change
  useEffect(() => {
    if (!backendOnline) { setRerankerStatus(null); return; }
    let active = true;
    getRerankerStatus()
      .then(s => { if (active) setRerankerStatus(s); })
      .catch(() => { if (active) setRerankerStatus({ available: false, loaded: false, available_variants: [], model_path: '' }); });
    return () => { active = false; };
  }, [backendOnline]);

  // Progress of the running reranked query, one event per finished chunk.
  // Subscribed for the view's lifetime, not the query's: MainArea keeps this
  // view mounted while another tab is in front, and a per-query subscription
  // would race the first event.
  useTauriEventListener('nl-rerank-progress', (event) => {
    setProgress(event.payload || null);
  });

  const handleSubmit = useCallback(async (e) => {
    e?.preventDefault?.();
    const trimmed = query.trim();
    if (!trimmed || !backendOnline) return;

    setLoading(true);
    inFlightRef.current = true;
    setError(null);
    setResults([]);
    setProgress(null);
    setStopping(false);
    setCancelled(false);
    // In calibrate mode, clear selection when starting a fresh query against
    // a different anchor — but preserve it if the same query is re-run. The
    // cached scores go with it: they are what the saved threshold is derived
    // from, and keeping scores from an earlier query would let one threshold
    // mix numbers produced by two different backends.
    if (isCalibrate && trimmed !== lastQuery) {
      setSelection({});
      setScoreById({});
    }
    try {
      const { results: out, reranked: didRerank, rerank_variant: usedVariant, backend, cancelled: wasCancelled } =
        await nlClusterQuery(trimmed, nResults, enableRerank);
      // A stopped query returns no ranking, so nothing about the previous one
      // is replaced — including `lastQuery`, which is what decides whether the
      // next attempt keeps the marks the user has already made.
      if (wasCancelled) {
        setCancelled(true);
        return;
      }
      setResults(out);
      setReranked(didRerank);
      setActiveVariant(usedVariant);
      setLastQuery(trimmed);
      setLastBackend(backend);
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
      inFlightRef.current = false;
      setLoading(false);
      setStopping(false);
      setProgress(null);
    }
  }, [query, nResults, enableRerank, backendOnline, isCalibrate, lastQuery]);

  /**
   * Stop the running query. The backend checks between chunks of eight
   * documents, so the button stays in its "stopping" state until the query's
   * own promise settles — which is what clears `loading`.
   *
   * Resolving `false` means there was nothing to stop, which on this screen has
   * one cause: Python is answering. Its rerank is a single opaque IPC call with
   * no chunk boundary to stop at, so the honest response is to say so and put
   * the button back rather than leave it spinning on a request that will never
   * be honoured.
   */
  const handleStop = useCallback(async () => {
    setStopping(true);
    try {
      const stopped = await nlRerankStopNow();
      if (!stopped) setStopping(false);
    } catch (err) {
      console.warn('Failed to stop the reranked query:', err);
      setStopping(false);
    }
  }, []);

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
        // Whose logits this threshold was computed from. Not the configured
        // backend — the one that answered.
        scorer_backend: lastBackend,
      });
      if (mountedRef.current) {
        // Reset on success
        setSelection({});
        setResults([]);
        setQuery('');
        setLastQuery('');
        setLastBackend(null);
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

  /**
   * How far the reranking phase has got, or `null` when there is no denominator
   * to divide by.
   *
   * Only the reranking phase has one. Retrieval is milliseconds and the model
   * load reports no ratio at all — it is one 544 MB read whose duration depends
   * on the disk, and a fabricated percentage there would be the same lie the
   * old static line told, only with a number attached.
   */
  const progressPercent = useMemo(() => {
    if (progress?.phase !== 'reranking') return null;
    const total = Number(progress.total) || 0;
    if (total <= 0) return null;
    const scored = Number(progress.scored) || 0;
    return Math.min(100, Math.round((scored / total) * 100));
  }, [progress]);

  /**
   * What the wait is currently spent on.
   *
   * Replaces "encode -> retrieve -> load reranker -> rerank", which named the
   * whole pipeline at every moment and therefore located the user at none of
   * them. Each phase says which of those four steps is actually running, and
   * the reranking one carries the count, because that is the step that takes
   * minutes.
   */
  const progressLabel = useMemo(() => {
    if (!enableRerank) return t('nlCluster.loadingSearching', '正在编码查询并匹配快照…');
    switch (progress?.phase) {
      case 'retrieving':
        return t('nlCluster.progressRetrieving', '正在召回候选快照…');
      case 'loading_model':
        return t('nlCluster.progressLoadingModel', '正在加载重排模型…');
      case 'reranking':
        return t('nlCluster.progressReranking', '正在重排候选 {{scored}}/{{total}}', {
          scored: Number(progress.scored) || 0,
          total: Number(progress.total) || 0,
        });
      case 'external_backend':
        return t('nlCluster.progressExternalBackend', '正在由 Python 后端重排，这一路径不报告进度…');
      default:
        return t('nlCluster.progressStarting', '正在准备…');
    }
  }, [enableRerank, progress, t]);

  /**
   * Python's rerank has no chunk boundary to stop at, so the button is hidden
   * rather than shown and refused. Everything else — including the moments
   * before the first progress event — is stoppable.
   */
  const canStop = enableRerank && progress?.phase !== 'external_backend';

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
            {RESULT_LIMITS.map((limit) => (
              <option key={limit} value={limit}>
                {t('nlCluster.topLimit', 'top {{count}}', { count: limit })}
              </option>
            ))}
          </select>
          {loading && canStop ? (
            <button
              type="button"
              onClick={handleStop}
              disabled={stopping}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-border text-ide-muted hover:text-ide-text hover:bg-ide-hover/40 disabled:opacity-40 transition-colors"
              title={t('nlCluster.stopTooltip', '停止本次检索')}
            >
              <Square className="w-3 h-3" />
              {stopping ? t('nlCluster.stopping', '正在停止…') : t('nlCluster.stop', '停止')}
            </button>
          ) : (
            <button
              type="submit"
              disabled={loading || !backendOnline || !query.trim()}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-accent bg-ide-accent/20 text-ide-accent hover:bg-ide-accent/30 disabled:opacity-40 transition-colors"
            >
              {loading ? <Loader2 className="w-3 h-3 animate-spin" /> : <Search className="w-3 h-3" />}
              {isCalibrate ? t('nlCluster.previewCandidates', '预览候选') : t('nlCluster.search', '检索')}
            </button>
          )}
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
          <div className="flex flex-col items-center justify-center h-40 gap-3 text-ide-muted">
            <Loader2 className="w-5 h-5 animate-spin" />
            <div className="w-64 max-w-full flex flex-col gap-1.5">
              <div className="flex items-baseline justify-between gap-2 text-xs">
                <span>{progressLabel}</span>
                {progressPercent !== null && (
                  <span className="font-mono text-[11px] tabular-nums text-ide-text">
                    {progressPercent}%
                  </span>
                )}
              </div>
              <div className="h-1 w-full rounded-full bg-ide-border/60 overflow-hidden">
                {progressPercent !== null && (
                  <div
                    className="h-full rounded-full bg-ide-accent transition-[width] duration-300 ease-out"
                    style={{ width: `${progressPercent}%` }}
                  />
                )}
              </div>
              {stopping && (
                <span className="text-[11px] opacity-70">
                  {t('nlCluster.stoppingHint', '正在停止，最多再等一个批次…')}
                </span>
              )}
            </div>
          </div>
        ) : !results.length ? (
          <div className="flex flex-col items-center justify-center h-40 gap-2 text-ide-muted text-sm">
            <Sparkles className="w-6 h-6 opacity-40" />
            <span>
              {cancelled
                ? t('nlCluster.cancelledNotice', '已停止本次检索，还没有结果')
                : lastQuery
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
