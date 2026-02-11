import React, { useState, useEffect, useMemo } from 'react';
import { HardDrive, RefreshCw, Clock, Database, Image as ImageIcon, Activity, Trash2, AlertTriangle } from 'lucide-react';
import { formatBytes, formatTimestamp } from './analysisUtils';

// Storage Ring Chart Component
function StorageRingChart({ totalDiskUsed, totalDiskSize, appUsedBytes, loading }) {
  const size = 180;
  const strokeWidth = 18;
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  
  // Calculate percentages
  const diskUsagePercent = totalDiskSize > 0 ? Math.min((totalDiskUsed / totalDiskSize) * 100, 100) : 0;
  const appUsagePercent = totalDiskSize > 0 ? Math.min((appUsedBytes / totalDiskSize) * 100, 100) : 0;
  
  // Calculate stroke dash offsets
  const diskStrokeDashoffset = circumference - (diskUsagePercent / 100) * circumference;
  const appStrokeDashoffset = circumference - (appUsagePercent / 100) * circumference;

  return (
    <div className="relative flex items-center justify-center">
      <svg width={size} height={size} className="transform -rotate-90">
        {/* Background ring */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke="currentColor"
          strokeWidth={strokeWidth}
          fill="none"
          className="text-ide-border/30"
        />
        {/* Disk usage ring (purple) */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke="url(#diskGradient)"
          strokeWidth={strokeWidth}
          fill="none"
          strokeDasharray={circumference}
          strokeDashoffset={diskStrokeDashoffset}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
        {/* App usage ring (blue) */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius - strokeWidth - 4}
          stroke="url(#appGradient)"
          strokeWidth={strokeWidth - 4}
          fill="none"
          strokeDasharray={circumference * ((radius - strokeWidth - 4) / radius)}
          strokeDashoffset={(circumference * ((radius - strokeWidth - 4) / radius)) - (appUsagePercent / 100) * (circumference * ((radius - strokeWidth - 4) / radius))}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
        <defs>
          <linearGradient id="diskGradient" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="#8B5CF6" />
            <stop offset="100%" stopColor="#A78BFA" />
          </linearGradient>
          <linearGradient id="appGradient" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="#3B82F6" />
            <stop offset="100%" stopColor="#60A5FA" />
          </linearGradient>
        </defs>
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center text-center">
        {loading ? (
          <RefreshCw className="w-6 h-6 animate-spin text-ide-muted" />
        ) : (
          <>
            <span className="text-2xl font-bold">{formatBytes(appUsedBytes)}</span>
            <span className="text-xs text-ide-muted">程序占用</span>
          </>
        )}
      </div>
    </div>
  );
}

// Storage option selector component
function StorageOptionSelect({ label, value, options, onChange, icon: Icon, description }) {
  return (
    <div className="bg-ide-bg/70 border border-ide-border rounded-xl p-4">
      <div className="flex items-center gap-3 mb-3">
        {Icon && (
          <div className="p-2 rounded-lg bg-ide-panel border border-ide-border">
            <Icon className="w-4 h-4" />
          </div>
        )}
        <div className="flex-1">
          <div className="font-medium text-sm">{label}</div>
          {description && <div className="text-xs text-ide-muted mt-0.5">{description}</div>}
        </div>
      </div>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full bg-ide-panel border border-ide-border rounded-lg px-3 py-2 text-sm text-ide-text focus:outline-none focus:border-ide-accent cursor-pointer"
      >
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
    </div>
  );
}

export default function StorageManagementSection({
  storageSegments,
  totalStorage,
  storage,
  loading,
  refreshing,
  error,
  onRefresh,
}) {
  // Storage settings from localStorage
  const [storageLimit, setStorageLimit] = useState(() => {
    return localStorage.getItem('snapshotStorageLimit') || 'unlimited';
  });
  
  const [retentionPeriod, setRetentionPeriod] = useState(() => {
    return localStorage.getItem('snapshotRetentionPeriod') || 'permanent';
  });

  // Save settings to localStorage
  useEffect(() => {
    localStorage.setItem('snapshotStorageLimit', storageLimit);
  }, [storageLimit]);

  useEffect(() => {
    localStorage.setItem('snapshotRetentionPeriod', retentionPeriod);
  }, [retentionPeriod]);

  // Storage limit options
  const storageLimitOptions = [
    { value: '10', label: '10 GB' },
    { value: '20', label: '20 GB' },
    { value: '50', label: '50 GB' },
    { value: '120', label: '120 GB' },
    { value: 'unlimited', label: '不限制' },
  ];

  // Retention period options
  const retentionOptions = [
    { value: '1month', label: '1 个月' },
    { value: '6months', label: '6 个月' },
    { value: '1year', label: '1 年' },
    { value: '2years', label: '2 年' },
    { value: 'permanent', label: '永久' },
  ];

  // Mock disk info - in real implementation this would come from backend
  const diskInfo = useMemo(() => {
    // Extract disk path from storage root path
    const rootPath = storage?.root_path || '';
    const driveLetter = rootPath.charAt(0);
    
    // For demo purposes, estimate disk usage
    // In production, this would come from a Rust backend call
    return {
      driveLetter: driveLetter || 'C',
      totalSize: 500 * 1024 * 1024 * 1024, // 500GB placeholder
      usedSize: 320 * 1024 * 1024 * 1024,   // 320GB placeholder
    };
  }, [storage]);

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between shrink-0">
        <div className="space-y-1">
          <h2 className="text-xl font-semibold">存储管理</h2>
          <p className="text-xs text-ide-muted">管理快照存储空间与数据保留策略</p>
        </div>
        <button
          type="button"
          onClick={onRefresh}
          disabled={refreshing}
          className="flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          刷新
        </button>
      </div>

      {error && (
        <div className="shrink-0 px-4 py-2 rounded-lg border border-red-500/40 text-xs text-red-200 bg-red-500/10">
          {error}
        </div>
      )}

      {/* Storage Ring Chart Card */}
      <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6">
        <div className="flex items-center gap-2 mb-6">
          <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
            <HardDrive className="w-4 h-4" />
          </div>
          <div>
            <h3 className="font-semibold">存储空间概览</h3>
            <p className="text-[11px] text-ide-muted">{storage?.root_path || 'LocalAppData/CarbonPaper'}</p>
          </div>
        </div>

        <div className="flex flex-col lg:flex-row items-center gap-8">
          {/* Ring Chart */}
          <div className="flex-shrink-0">
            <StorageRingChart
              totalDiskUsed={diskInfo.usedSize}
              totalDiskSize={diskInfo.totalSize}
              appUsedBytes={totalStorage}
              loading={loading}
            />
          </div>

          {/* Legend and Stats */}
          <div className="flex-1 space-y-4">
            {/* Legend */}
            <div className="space-y-2">
              <div className="flex items-center gap-3">
                <div className="w-3 h-3 rounded-full bg-gradient-to-r from-purple-500 to-purple-400" />
                <span className="text-sm text-ide-muted">硬盘总占用</span>
                <span className="text-sm font-medium ml-auto">{formatBytes(diskInfo.usedSize)}</span>
              </div>
              <div className="flex items-center gap-3">
                <div className="w-3 h-3 rounded-full bg-gradient-to-r from-blue-500 to-blue-400" />
                <span className="text-sm text-ide-muted">程序占用</span>
                <span className="text-sm font-medium ml-auto">{formatBytes(totalStorage)}</span>
              </div>
            </div>

            {/* Detailed breakdown */}
            <div className="grid grid-cols-2 gap-2 pt-2 border-t border-ide-border/50">
              {storageSegments.map((segment) => {
                const Icon = segment.icon;
                return (
                  <div key={segment.key} className="flex items-center gap-2 text-xs">
                    <Icon className="w-3.5 h-3.5 text-ide-muted" />
                    <span className="text-ide-muted">{segment.label}</span>
                    <span className="ml-auto font-medium">{formatBytes(segment.bytes)}</span>
                  </div>
                );
              })}
            </div>

            <div className="text-[11px] text-ide-muted pt-2">
              更新时间: {storage?.cached_at_ms ? formatTimestamp(storage.cached_at_ms) : '--'}
            </div>
          </div>
        </div>
      </div>

      {/* Storage Settings */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <StorageOptionSelect
          label="快照存储空间上限"
          description="超出后将自动删除最旧的快照"
          value={storageLimit}
          onChange={setStorageLimit}
          options={storageLimitOptions}
          icon={Database}
        />
        
        <StorageOptionSelect
          label="快照保留时间"
          description="超过保留时间的快照将被自动清理"
          value={retentionPeriod}
          onChange={setRetentionPeriod}
          options={retentionOptions}
          icon={Clock}
        />
      </div>

      {/* Warning for unlimited storage */}
      {storageLimit === 'unlimited' && retentionPeriod === 'permanent' && (
        <div className="flex items-start gap-3 px-4 py-3 rounded-lg border border-amber-500/40 bg-amber-500/10">
          <AlertTriangle className="w-4 h-4 text-amber-400 mt-0.5 shrink-0" />
          <div className="text-xs text-yellow-600 dark:text-yellow-500">
            <p className="font-medium mb-1">警告</p>
            <p>
              当前设置为不限制存储空间且永久保留，快照文件可能会占用大量磁盘空间。
              建议设置合理的存储上限或保留时间。
            </p>
          </div>
        </div>
      )}
    </div>
  );
}
