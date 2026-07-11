import React from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw } from 'lucide-react';
import { formatBytes } from '../analysisUtils';

export default function StorageRingChart({
  totalDiskUsed,
  totalDiskSize,
  appUsedBytes,
  loading,
}) {
  const { t } = useTranslation();
  const size = 180;
  const strokeWidth = 18;
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;

  const diskUsagePercent = totalDiskSize > 0 ? Math.min((totalDiskUsed / totalDiskSize) * 100, 100) : 0;
  const appUsagePercent = totalDiskSize > 0 ? Math.min((appUsedBytes / totalDiskSize) * 100, 100) : 0;
  const diskStrokeDashoffset = circumference - (diskUsagePercent / 100) * circumference;
  const appCircumference = circumference * ((radius - strokeWidth - 4) / radius);
  const appStrokeDashoffset = appCircumference - (appUsagePercent / 100) * appCircumference;

  return (
    <div className="relative flex items-center justify-center">
      <svg width={size} height={size} className="transform -rotate-90">
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke="currentColor"
          strokeWidth={strokeWidth}
          fill="none"
          className="text-ide-border/30"
        />
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
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius - strokeWidth - 4}
          stroke="url(#appGradient)"
          strokeWidth={strokeWidth - 4}
          fill="none"
          strokeDasharray={appCircumference}
          strokeDashoffset={appStrokeDashoffset}
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
            <span className="text-xs text-ide-muted">{t('settings.storageManagement.overview.program_used')}</span>
          </>
        )}
      </div>
    </div>
  );
}
