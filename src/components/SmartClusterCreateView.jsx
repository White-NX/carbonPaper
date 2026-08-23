import React, { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Search, Loader2, Sparkles, AlertCircle, X, Square,
  Check, ArrowLeft, ArrowRight, Info,
} from 'lucide-react';
import { nlClusterQuery, nlRerankStopNow, getRerankerStatus } from '../lib/task_api';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';
import { PageHeader } from './PageHeader';
import { useTauriEventListener } from '../hooks/useTauriEventListener';

/**
 * 想收集什么的例子。
 *
 * 换掉了原先从模型评测语料里搬来的三条，改成用户真的会写出来的句子：这些
 * 文字会被当成语义锚点直接拿去匹配，示例写成什么样，用户就会照着写成什么样。
 */
const EXAMPLES = [
  { key: 'smartClusterCreate.exampleReceipts', fallback: '网上购物的订单和收据' },
  { key: 'smartClusterCreate.exampleTravel', fallback: '旅行计划和行程安排' },
  { key: 'smartClusterCreate.exampleReading', fallback: '读到的长文章和技术资料' },
];

/**
 * 至少要选中多少张，才能定出一条可用的匹配线。
 *
 * 少于这个数量时，最低分那一张的偶然性会直接变成整个聚类的松紧程度，之后
 * 每一张新画面都按它来判断。
 */
const MIN_KEEPS = 3;

/**
 * 一次候选检索取多少张。
 *
 * 后端对带重排的检索本来就会截到 30（`rerank.rs::MAX_RERANK_RESULTS`），
 * 每张候选都要过一遍 CPU 上的交叉编码器，再多也只是让用户白等。
 */
const CANDIDATE_COUNT = 30;

/** 命名步骤里可选的颜色，用来在列表里区分不同聚类。 */
const COLOR_CHOICES = [
  '#6366f1', '#0ea5e9', '#10b981', '#f59e0b',
  '#ef4444', '#ec4899', '#8b5cf6', '#64748b',
];

/**
 * 从描述文字推出一个稳定的颜色，作为默认值。
 *
 * 同样的描述永远得到同样的颜色，所以用户重来一次时看到的默认值不会跳。
 */
function colorFromText(text) {
  let hash = 2166136261 >>> 0;
  for (let i = 0; i < text.length; i++) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 16777619) >>> 0;
  }
  return COLOR_CHOICES[hash % COLOR_CHOICES.length];
}

/**
 * 从用户选中和排除的例子里定出匹配线。
 *
 *   匹配线 = 选中项里最低的那个分数 × 0.85
 *   如果有被排除的项分数还在这条线以上，就抬到 最高排除分 × 1.05
 *
 * 分数本身不出现在界面上，但这条线必须由它们算出来：用户认可的那几张就是
 * 「够像」的下界，被划掉的那几张是「还不够像」的上界。
 */
function computeThreshold(keepScores, skipScores) {
  const keeps = keepScores.filter((s) => typeof s === 'number');
  const skips = skipScores.filter((s) => typeof s === 'number');
  if (!keeps.length) return null;
  const base = Math.min(...keeps) * 0.85;
  if (!skips.length) return base;
  return Math.max(base, Math.max(...skips) * 1.05);
}

/**
 * 创建智能聚类的三步流程：描述想收集什么、挑几个例子、起名字。
 *
 * 这三步不只是把一屏拆成三屏。第二步挑出来的例子决定了这个聚类以后的松紧
 * 程度，第一步那句描述会被原样当成语义锚点保存下来，两者都不是能事后随便
 * 改的东西，所以各自占一屏，让用户一次只决定一件事。
 *
 * 第三步的名字则相反，它纯粹是个标签，改了不影响这个聚类收什么——后端从
 * `display_name` 和 `anchor_text` 分开的那一刻起就是这样。
 *
 * @param {object} props
 * @param {(item: object) => void} [props.onSelectScreenshot] 点开某张候选
 * @param {(req: object) => Promise<void>} props.onSave 提交创建
 * @param {() => void} props.onCancel 放弃创建并返回列表
 */
export default function SmartClusterCreateView({ onSelectScreenshot, onSave, onCancel }) {
  const { t } = useTranslation();

  const [step, setStep] = useState('describe');
  const [description, setDescription] = useState('');
  // 产生当前这批候选的那句描述。用户可能改了输入框却还没重新检索，锚点必须
  // 取真正跑过检索的那一句。
  const [committed, setCommitted] = useState('');
  const [results, setResults] = useState([]);
  const [selection, setSelection] = useState({});
  const [scoreById, setScoreById] = useState({});
  const [name, setName] = useState('');
  const [color, setColor] = useState(COLOR_CHOICES[0]);

  const [loading, setLoading] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [cancelled, setCancelled] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState(null);
  const [progress, setProgress] = useState(null);
  const [thumbnailCache, setThumbnailCache] = useState({});
  const [modelReady, setModelReady] = useState(true);

  const mountedRef = useRef(true);
  const inFlightRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      // 关掉页面并不会让后台停下来。不主动喊停的话，它会继续算上好几分钟，
      // 而且下一次检索还得和它抢同一个工作线程。
      if (inFlightRef.current) nlRerankStopNow().catch(() => {});
    };
  }, []);

  useEffect(() => {
    let active = true;
    getRerankerStatus()
      .then((s) => { if (active) setModelReady(Boolean(s?.available)); })
      .catch(() => { if (active) setModelReady(false); });
    return () => { active = false; };
  }, []);

  useTauriEventListener('nl-rerank-progress', (event) => {
    setProgress(event.payload || null);
  });

  useEffect(() => {
    if (!results.length) return undefined;
    let active = true;
    const ids = [...new Set(results.map((r) => r.screenshot_id).filter((id) => id > 0))];
    fetchThumbnailBatch(ids)
      .then((batch) => { if (active && batch) setThumbnailCache((prev) => ({ ...prev, ...batch })); })
      .catch((err) => console.error('thumbnail batch failed:', err));
    return () => { active = false; };
  }, [results]);

  const runSearch = useCallback(async (event) => {
    event?.preventDefault?.();
    const trimmed = description.trim();
    if (!trimmed || loading) return;

    setLoading(true);
    inFlightRef.current = true;
    setError(null);
    setResults([]);
    setProgress(null);
    setStopping(false);
    setCancelled(false);
    setStep('pick');
    // 换了描述就等于换了一个聚类，之前标过的那些不再属于它；分数也一起丢掉，
    // 否则匹配线会由两批不同检索的分数混着算出来。
    if (trimmed !== committed) {
      setSelection({});
      setScoreById({});
    }

    try {
      const { results: out, cancelled: wasCancelled } =
        await nlClusterQuery(trimmed, CANDIDATE_COUNT, true);
      if (!mountedRef.current) return;
      if (wasCancelled) {
        setCancelled(true);
        return;
      }
      setResults(out);
      setCommitted(trimmed);
      const scores = {};
      for (const r of out) {
        if (r.rerank_score !== undefined) scores[r.screenshot_id] = r.rerank_score;
      }
      setScoreById((prev) => ({ ...prev, ...scores }));
    } catch (err) {
      if (mountedRef.current) setError(err?.message || String(err));
      console.error('nl_cluster_query failed:', err);
    } finally {
      inFlightRef.current = false;
      if (mountedRef.current) {
        setLoading(false);
        setStopping(false);
        setProgress(null);
      }
    }
  }, [description, committed, loading]);

  const handleStop = useCallback(async () => {
    setStopping(true);
    try {
      const stopped = await nlRerankStopNow();
      if (!stopped) setStopping(false);
    } catch (err) {
      console.warn('Failed to stop the running search:', err);
      setStopping(false);
    }
  }, []);

  const toggleMark = (screenshotId, kind) => {
    setSelection((prev) => {
      const next = { ...prev };
      if (next[screenshotId] === kind) delete next[screenshotId];
      else next[screenshotId] = kind;
      return next;
    });
  };

  const counts = useMemo(() => {
    let keep = 0;
    let skip = 0;
    for (const value of Object.values(selection)) {
      if (value === 'keep') keep += 1;
      else if (value === 'skip') skip += 1;
    }
    return { keep, skip };
  }, [selection]);

  const enterNaming = () => {
    setError(null);
    setName((prev) => prev || committed);
    setColor(colorFromText(committed));
    setStep('name');
  };

  const handleCreate = async () => {
    if (saving) return;
    const keeps = Object.entries(selection).filter(([, v]) => v === 'keep');
    const skips = Object.entries(selection).filter(([, v]) => v === 'skip');

    const threshold = computeThreshold(
      keeps.map(([id]) => scoreById[Number(id)]),
      skips.map(([id]) => scoreById[Number(id)]),
    );
    if (threshold === null || Number.isNaN(threshold)) {
      setError(t('smartClusterCreate.errorNoThreshold', '候选样本评分数据异常，请返回上一步重新检索'));
      return;
    }

    setSaving(true);
    try {
      await onSave({
        anchor_text: committed,
        display_name: name.trim() || committed,
        threshold,
        dominant_color: color,
        examples: [
          ...keeps.map(([id]) => ({
            screenshot_id: Number(id),
            is_positive: true,
            rerank_score: scoreById[Number(id)],
          })),
          ...skips.map(([id]) => ({
            screenshot_id: Number(id),
            is_positive: false,
            rerank_score: scoreById[Number(id)],
          })),
        ],
        scorer_backend: 'rust',
      });
    } catch (err) {
      if (mountedRef.current) setError(err?.message || String(err));
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  /**
   * 标定这一步进行到哪儿了。
   *
   * 只有重排阶段有分母可以除。召回是毫秒级的，而加载模型报不出比例——那是
   * 一次几百兆的磁盘读取，快慢取决于硬盘，在那里编一个百分比出来，和过去那
   * 句一动不动的静态提示是同一种糊弄，只不过多带了个数字。
   */
  const progressPercent = useMemo(() => {
    if (progress?.phase !== 'reranking') return null;
    const total = Number(progress.total) || 0;
    if (total <= 0) return null;
    return Math.min(100, Math.round(((Number(progress.scored) || 0) / total) * 100));
  }, [progress]);

  const progressLabel = useMemo(() => {
    switch (progress?.phase) {
      case 'retrieving':
        return t('smartClusterCreate.progressRetrieving', '正在召回候选快照…');
      case 'loading_model':
        return t('smartClusterCreate.progressLoadingModel', '正在准备…');
      case 'reranking':
        return t('smartClusterCreate.progressPicking', '正在重排候选快照 {{scored}}/{{total}}', {
          scored: Number(progress.scored) || 0,
          total: Number(progress.total) || 0,
        });
      default:
        return t('smartClusterCreate.progressStarting', '正在准备…');
    }
  }, [progress, t]);

  const stepIndex = step === 'describe' ? 0 : step === 'pick' ? 1 : 2;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <PageHeader
        as={step === 'describe' ? 'form' : 'div'}
        onSubmit={step === 'describe' ? runSearch : undefined}
        secondaryRow={<StepTrail current={stepIndex} t={t} />}
      >
        {step === 'describe' ? (
          <>
            <div className="flex h-10 w-full max-w-[620px] items-center gap-1 rounded-lg border border-ide-border bg-ide-bg pl-3.5 pr-1.5 transition-colors focus-within:border-ide-accent focus-within:ring-2 focus-within:ring-ide-accent/20">
              <input
                type="text"
                autoFocus
                value={description}
                onChange={(event) => setDescription(event.target.value)}
                placeholder={t('smartClusterCreate.placeholder', '输入自然语言描述（如：网上购物的订单和收据）')}
                className="min-w-0 flex-1 bg-transparent text-sm text-ide-text outline-none placeholder:text-ide-muted"
              />
              {description && (
                <button
                  type="button"
                  onClick={() => setDescription('')}
                  title={t('common.clear', '清空')}
                  className="grid h-7 w-7 place-items-center rounded-md text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              )}
              <button
                type="submit"
                disabled={!description.trim() || !modelReady}
                title={t('smartClusterCreate.find', '检索候选')}
                className="grid h-[30px] w-[30px] shrink-0 place-items-center rounded-md bg-ide-accent text-white transition hover:brightness-110 disabled:opacity-40"
              >
                <Search className="h-4 w-4" />
              </button>
            </div>
            <button
              type="button"
              onClick={onCancel}
              className="ml-auto shrink-0 rounded px-3 py-1.5 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
            >
              {t('common.cancel', '取消')}
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              onClick={() => (step === 'name' ? setStep('pick') : setStep('describe'))}
              disabled={saving}
              className="flex shrink-0 items-center gap-1.5 rounded px-2 py-1.5 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text disabled:opacity-40"
            >
              <ArrowLeft className="h-3.5 w-3.5" />
              {t('common.back', '上一步')}
            </button>
            <span className="min-w-0 flex-1 truncate text-sm text-ide-text" title={committed}>
              {committed}
            </span>
            {step === 'pick' && loading && (
              <button
                type="button"
                onClick={handleStop}
                disabled={stopping}
                className="flex shrink-0 items-center gap-1.5 rounded border border-ide-border px-3 py-1.5 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text disabled:opacity-40"
              >
                <Square className="h-3 w-3" />
                {stopping ? t('smartClusterCreate.stopping', '正在停止…') : t('smartClusterCreate.stop', '停止')}
              </button>
            )}
            <button
              type="button"
              onClick={onCancel}
              disabled={saving}
              className="shrink-0 rounded px-3 py-1.5 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text disabled:opacity-40"
            >
              {t('common.cancel', '取消')}
            </button>
          </>
        )}
      </PageHeader>

      {error && (
        <div className="mx-6 mt-3 flex shrink-0 items-center gap-2 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2">
          <AlertCircle className="h-3.5 w-3.5 shrink-0 text-red-400" />
          <span className="flex-1 break-all text-xs text-red-400">{error}</span>
          <button onClick={() => setError(null)} className="text-red-400 hover:text-red-300">
            <X className="h-3 w-3" />
          </button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto custom-scrollbar">
        {step === 'describe' && (
          <DescribeStep
            modelReady={modelReady}
            onPick={(text) => setDescription(text)}
            t={t}
          />
        )}

        {step === 'pick' && (
          <PickStep
            loading={loading}
            cancelled={cancelled}
            results={results}
            selection={selection}
            thumbnailCache={thumbnailCache}
            progressLabel={progressLabel}
            progressPercent={progressPercent}
            stopping={stopping}
            onToggle={toggleMark}
            onSelectScreenshot={onSelectScreenshot}
            t={t}
          />
        )}

        {step === 'name' && (
          <NameStep
            name={name}
            onNameChange={setName}
            color={color}
            onColorChange={setColor}
            keepCount={counts.keep}
            description={committed}
            t={t}
          />
        )}
      </div>

      {step === 'pick' && !loading && results.length > 0 && (
        <div className="flex shrink-0 items-center gap-3 border-t border-ide-border bg-ide-panel px-6 py-3">
          <span className="text-xs text-ide-muted">
            {counts.keep >= MIN_KEEPS
              ? t('smartClusterCreate.readyHint', '已选 {{count}} 张正例', { count: counts.keep })
              : t('smartClusterCreate.needMore', '还需标注 {{count}} 张正例', {
                count: MIN_KEEPS - counts.keep,
              })}
          </span>
          <button
            type="button"
            onClick={enterNaming}
            disabled={counts.keep < MIN_KEEPS}
            className="ml-auto flex items-center gap-1.5 rounded-md bg-ide-accent px-4 py-1.5 text-xs font-medium text-white transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {t('common.next', '下一步')}
            <ArrowRight className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {step === 'name' && (
        <div className="flex shrink-0 items-center justify-end gap-2 border-t border-ide-border bg-ide-panel px-6 py-3">
          <button
            type="button"
            onClick={onCancel}
            disabled={saving}
            className="rounded px-3 py-1.5 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text disabled:opacity-40"
          >
            {t('common.cancel', '取消')}
          </button>
          <button
            type="button"
            onClick={handleCreate}
            disabled={saving || !name.trim()}
            className="flex items-center gap-1.5 rounded-md bg-ide-accent px-4 py-1.5 text-xs font-medium text-white transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Check className="h-3.5 w-3.5" />}
            {t('smartClusterCreate.create', '创建')}
          </button>
        </div>
      )}
    </div>
  );
}

/** 顶栏第二行的三步指示。 */
function StepTrail({ current, t }) {
  const labels = [
    t('smartClusterCreate.step1', '描述'),
    t('smartClusterCreate.step2', '标定'),
    t('smartClusterCreate.step3', '命名'),
  ];
  return (
    <div className="flex items-center gap-2">
      {labels.map((label, index) => (
        <React.Fragment key={label}>
          {index > 0 && <span className="h-px w-5 bg-ide-border" aria-hidden="true" />}
          <span
            className={`flex items-center gap-1.5 text-xs ${
              index === current ? 'font-medium text-ide-accent' : 'text-ide-muted'
            }`}
          >
            <span
              className={`grid h-[18px] w-[18px] place-items-center rounded-full text-[10px] ${
                index < current
                  ? 'bg-ide-accent/20 text-ide-accent'
                  : index === current
                    ? 'bg-ide-accent text-white'
                    : 'border border-ide-border text-ide-muted'
              }`}
            >
              {index < current ? <Check className="h-2.5 w-2.5" /> : index + 1}
            </span>
            {label}
          </span>
        </React.Fragment>
      ))}
    </div>
  );
}

function DescribeStep({ modelReady, onPick, t }) {
  return (
    <div className="mx-auto max-w-2xl px-6 py-10">
      <p className="text-sm leading-relaxed text-ide-text/90">
        {t(
          'smartClusterCreate.intro',
          '输入描述并标记样本，程序将自动匹配并归档历史与未来的相关快照。',
        )}
      </p>

      {!modelReady && (
        <div className="mt-5 flex items-start gap-2.5 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3">
          <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-amber-400" />
          <p className="text-xs leading-relaxed text-amber-400">
            {t(
              'smartClusterCreate.modelMissing',
              '智能聚类重排模型尚未就绪，请前往「设置 → 功能管理」下载依赖组件。',
            )}
          </p>
        </div>
      )}

      <p className="mt-8 text-xs text-ide-muted">{t('smartClusterCreate.examplesTitle', '示例描述')}</p>
      <div className="mt-2.5 grid gap-2 sm:grid-cols-3">
        {EXAMPLES.map(({ key, fallback }) => {
          const text = t(key, fallback);
          return (
            <button
              key={key}
              type="button"
              onClick={() => onPick(text)}
              className="rounded-lg border border-ide-border bg-ide-panel/60 px-3 py-2.5 text-left text-xs leading-relaxed text-ide-muted transition-colors hover:border-ide-accent/50 hover:bg-ide-hover/40 hover:text-ide-text"
            >
              {text}
            </button>
          );
        })}
      </div>

      <div className="mt-8 flex items-start gap-2 text-[11px] leading-relaxed text-ide-muted/80">
        <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <p>
          {t(
            'smartClusterCreate.scopeNote',
            '候选样本取自近 30 天快照记录；后台归档仅在系统空闲时执行。',
          )}
        </p>
      </div>
    </div>
  );
}

function PickStep({
  loading, cancelled, results, selection, thumbnailCache,
  progressLabel, progressPercent, stopping, onToggle, onSelectScreenshot, t,
}) {
  if (loading) {
    return (
      <div className="flex h-40 flex-col items-center justify-center gap-3 text-ide-muted">
        <Loader2 className="h-5 w-5 animate-spin" />
        <div className="flex w-64 max-w-full flex-col gap-1.5">
          <div className="flex items-baseline justify-between gap-2 text-xs">
            <span>{progressLabel}</span>
            {progressPercent !== null && (
              <span className="font-mono text-[11px] tabular-nums text-ide-text">{progressPercent}%</span>
            )}
          </div>
          <div className="h-1 w-full overflow-hidden rounded-full bg-ide-border/60">
            {progressPercent !== null && (
              <div
                className="h-full rounded-full bg-ide-accent transition-[width] duration-300 ease-out"
                style={{ width: `${progressPercent}%` }}
              />
            )}
          </div>
          {stopping && (
            <span className="text-[11px] opacity-70">
              {t('smartClusterCreate.stoppingHint', '正在停止，等待当前批次完成…')}
            </span>
          )}
        </div>
      </div>
    );
  }

  if (!results.length) {
    return (
      <div className="flex h-40 flex-col items-center justify-center gap-2 text-sm text-ide-muted">
        <Sparkles className="h-6 w-6 opacity-40" />
        <span>
          {cancelled
            ? t('smartClusterCreate.stoppedNotice', '检索已停止，未产生结果')
            : t('smartClusterCreate.noResults', '未找到相关快照，请尝试调整描述内容')}
        </span>
      </div>
    );
  }

  return (
    <div className="px-6 py-4">
      <p className="mb-3.5 text-xs text-ide-muted">
        {t('smartClusterCreate.pickHint', '标记符合描述的快照（正例），排除无关快照（反例）。点击卡片可预览大图。')}
      </p>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
        {results.map((r) => {
          const mark = selection[r.screenshot_id];
          return (
            <div key={r.screenshot_id} className="group relative">
              <div className="absolute left-1 top-1 z-10 flex items-center gap-1">
                <button
                  type="button"
                  onClick={(event) => { event.stopPropagation(); onToggle(r.screenshot_id, 'keep'); }}
                  title={t('smartClusterCreate.markKeep', '正例')}
                  className={`rounded p-1 transition-all ${
                    mark === 'keep'
                      ? 'bg-emerald-500 text-white shadow-md'
                      : 'bg-black/50 text-white/70 opacity-0 hover:bg-emerald-500/70 group-hover:opacity-100'
                  }`}
                >
                  <Check className="h-3 w-3" />
                </button>
                <button
                  type="button"
                  onClick={(event) => { event.stopPropagation(); onToggle(r.screenshot_id, 'skip'); }}
                  title={t('smartClusterCreate.markSkip', '排除')}
                  className={`rounded p-1 transition-all ${
                    mark === 'skip'
                      ? 'bg-rose-500 text-white shadow-md'
                      : 'bg-black/50 text-white/70 opacity-0 hover:bg-rose-500/70 group-hover:opacity-100'
                  }`}
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
              {mark && (
                <div
                  className={`pointer-events-none absolute inset-0 z-[5] rounded border-2 ${
                    mark === 'keep' ? 'border-emerald-500' : 'border-rose-500'
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
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}

function NameStep({ name, onNameChange, color, onColorChange, keepCount, description, t }) {
  return (
    <div className="mx-auto max-w-lg px-6 py-10">
      <label className="block text-xs text-ide-muted" htmlFor="smart-cluster-name">
        {t('smartClusterCreate.nameLabel', '聚类名称')}
      </label>
      <input
        id="smart-cluster-name"
        type="text"
        autoFocus
        value={name}
        onChange={(event) => onNameChange(event.target.value)}
        className="mt-2 h-10 w-full rounded-lg border border-ide-border bg-ide-bg px-3.5 text-sm text-ide-text outline-none transition-colors focus:border-ide-accent focus:ring-2 focus:ring-ide-accent/20"
      />
      <p className="mt-2 text-[11px] leading-relaxed text-ide-muted/80">
        {t('smartClusterCreate.nameHint', '名称用于列表标识，修改名称不会变更匹配规则。')}
      </p>

      <p className="mt-7 text-xs text-ide-muted">{t('smartClusterCreate.colorLabel', '标记颜色')}</p>
      <div className="mt-2.5 flex flex-wrap gap-2">
        {COLOR_CHOICES.map((choice) => (
          <button
            key={choice}
            type="button"
            onClick={() => onColorChange(choice)}
            aria-label={choice}
            aria-pressed={color === choice}
            className={`h-7 w-7 rounded-md transition-transform ${
              color === choice ? 'ring-2 ring-ide-accent ring-offset-2 ring-offset-ide-bg' : 'hover:scale-110'
            }`}
            style={{ backgroundColor: choice }}
          />
        ))}
      </div>

      <div className="mt-8 rounded-lg border border-ide-border bg-ide-panel/60 p-4">
        <div className="flex items-center gap-2">
          <span className="h-2.5 w-2.5 rounded-[3px]" style={{ backgroundColor: color }} />
          <span className="truncate text-[13px] font-semibold text-ide-text">
            {name.trim() || description}
          </span>
        </div>
        <p className="mt-2 text-xs leading-relaxed text-ide-muted">
          {t(
            'smartClusterCreate.summary',
            '程序将基于所选的 {{count}} 张样本计算匹配阈值，自动归档后续捕获的相似快照。',
            { count: keepCount },
          )}
        </p>
      </div>
    </div>
  );
}
