import React, { useEffect, useMemo, useState } from 'react';
import { Activity, HardDrive, Image as ImageIcon, Database, RefreshCw } from 'lucide-react';
import { getAnalysisOverview } from '../lib/analysis_api';

const REFRESH_INTERVAL_MS = 30000;

const formatBytes = (bytes) => {
  if (bytes === null || bytes === undefined) return '--';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / Math.pow(1024, index);
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[index]}`;
};

const formatTimestamp = (ms) => {
  if (!ms) return '--';
  return new Date(ms).toLocaleString();
};

const buildLinePath = (points) => {
  if (!points || points.length === 0) return '';
  const times = points.map((p) => p.timestamp_ms);
  const values = points.map((p) => p.rss_bytes);
  const minTime = Math.min(...times);
  const maxTime = Math.max(...times);
  const minVal = Math.min(...values);
  const maxVal = Math.max(...values);
  const spanTime = Math.max(maxTime - minTime, 1);
  const spanVal = Math.max(maxVal - minVal, 1);

  return points
    .map((p, index) => {
      const x = ((p.timestamp_ms - minTime) / spanTime) * 100;
      const y = 100 - ((p.rss_bytes - minVal) / spanVal) * 100;
      return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
};

export function Analysis() {
  const [memorySeries, setMemorySeries] = useState([]);
  const [storage, setStorage] = useState(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState('');

  const loadOverview = async (forceStorage = false) => {
    try {
      setError('');
      const result = await getAnalysisOverview(forceStorage);
      setMemorySeries(result?.memory || []);
      setStorage(result?.storage || null);
    } catch (err) {
      setError(err?.message || 'Failed to load analysis data');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  };

  useEffect(() => {
    loadOverview(false);
    const timer = setInterval(() => {
      loadOverview(false);
    }, REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  }, []);

  const handleRefreshStorage = () => {
    setRefreshing(true);
    loadOverview(true);
  };

  const memoryStats = useMemo(() => {
    if (!memorySeries.length) return null;
    const values = memorySeries.map((point) => point.rss_bytes);
    const total = values.reduce((sum, value) => sum + value, 0);
    return {
      latest: values[values.length - 1],
      min: Math.min(...values),
      max: Math.max(...values),
      avg: Math.round(total / values.length),
      lastUpdated: memorySeries[memorySeries.length - 1]?.timestamp_ms
    };
  }, [memorySeries]);

  const storageSegments = useMemo(() => {
    if (!storage) return [];
    return [
      {
        key: 'models',
        label: '模型',
        bytes: storage.models_bytes,
        icon: Activity,
        color: 'bg-indigo-500/70'
      },
      {
        key: 'images',
        label: '图片',
        bytes: storage.images_bytes,
        icon: ImageIcon,
        color: 'bg-sky-500/70'
      },
      {
        key: 'database',
        label: '数据库',
        bytes: storage.database_bytes,
        icon: Database,
        color: 'bg-emerald-500/70'
      },
      {
        key: 'other',
        label: '程序关键依赖',
        bytes: storage.other_bytes,
        icon: HardDrive,
        color: 'bg-amber-500/70'
      }
    ];
  }, [storage]);

  const totalStorage = storage?.total_bytes || 0;

  return (
    <div className="flex flex-col w-full h-full p-8 gap-6 overflow-y-auto bg-ide-bg text-ide-text">
      <div className="flex items-center justify-between shrink-0">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold">Analysis</h2>
          <p className="text-xs text-ide-muted">最近半小时内的 Python 子服务资源视图与本地存储占用。</p>
          <p className="text-xs text-ide-muted">此处显示的内存占用为未压缩内存，实际占用约为未压缩值的1/7</p>
        </div>
        <button
          type="button"
          onClick={handleRefreshStorage}
          disabled={refreshing}
          className="flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          刷新存储
        </button>
      </div>

      {error && (
        <div className="shrink-0 px-4 py-2 rounded-lg border border-red-500/40 text-xs text-red-200 bg-red-500/10">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)] gap-6 w-full">
        <div className="flex flex-col gap-6">
          <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6 flex flex-col">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-2">
                <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                  <Activity className="w-4 h-4" />
                </div>
                <div>
                  <h3 className="font-semibold">Python 内存波动</h3>
                  <p className="text-[11px] text-ide-muted">最近 30 分钟</p>
                </div>
              </div>
              <div className="text-[11px] text-ide-muted">
                更新: {memoryStats ? formatTimestamp(memoryStats.lastUpdated) : '--'}
              </div>
            </div>

            <div className="h-64 w-full rounded-xl border border-ide-border bg-ide-bg/70 p-4 relative">
              {loading ? (
                <div className="absolute inset-0 flex items-center justify-center text-sm text-ide-muted">加载中...</div>
              ) : memorySeries.length === 0 ? (
                <div className="absolute inset-0 flex items-center justify-center text-sm text-ide-muted">暂无数据</div>
              ) : (
                <svg viewBox="0 0 100 100" preserveAspectRatio="none" className="w-full h-full">
                  <defs>
                    <linearGradient id="memoryGradient" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="rgba(59, 130, 246, 0.45)" />
                      <stop offset="100%" stopColor="rgba(59, 130, 246, 0)" />
                    </linearGradient>
                  </defs>
                  <path d={`${buildLinePath(memorySeries)} L 100 100 L 0 100 Z`} fill="url(#memoryGradient)" />
                  <path d={buildLinePath(memorySeries)} fill="none" stroke="#60A5FA" strokeWidth="1.5" />
                </svg>
              )}
            </div>

            <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 mt-4 text-xs">
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">当前</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.latest) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">最低</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.min) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">最高</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.max) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">平均</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.avg) : '--'}</div>
              </div>
            </div>
          </div>
        </div>

        <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6 flex flex-col">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                <HardDrive className="w-4 h-4" />
              </div>
              <div>
                <h3 className="font-semibold">本地存储占用</h3>
                <p className="text-[11px] text-ide-muted">LocalAppData/CarbonPaper</p>
              </div>
            </div>
            <div className="text-[11px] text-ide-muted">
              缓存: {storage?.cached_at_ms ? formatTimestamp(storage.cached_at_ms) : '--'}
            </div>
          </div>

          <div className="rounded-xl border border-ide-border bg-ide-bg/70 p-4 mb-4">
            <div className="text-xs text-ide-muted">总占用</div>
            <div className="text-2xl font-semibold mt-1">{formatBytes(totalStorage)}</div>
            <div className="text-[11px] text-ide-muted mt-1 truncate">{storage?.root_path || '--'}</div>
          </div>

          <div className="space-y-3">
            {storageSegments.map((segment) => {
              const percent = totalStorage ? Math.min((segment.bytes / totalStorage) * 100, 100) : 0;
              const Icon = segment.icon;
              return (
                <div key={segment.key} className="rounded-lg border border-ide-border bg-ide-bg/60 p-3">
                  <div className="flex items-center justify-between text-xs">
                    <div className="flex items-center gap-2">
                      <Icon className="w-3.5 h-3.5 text-ide-muted" />
                      <span className="text-ide-text">{segment.label}</span>
                    </div>
                    <span className="text-ide-muted">{formatBytes(segment.bytes)}</span>
                  </div>
                  <div className="mt-2 h-2 rounded-full bg-ide-border/60 overflow-hidden">
                    <div className={`h-full ${segment.color}`} style={{ width: `${percent}%` }} />
                  </div>
                  <div className="mt-1 text-[10px] text-ide-muted text-right">{percent.toFixed(1)}%</div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
