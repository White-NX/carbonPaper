import React from 'react';
import { Github, User, CheckCircle2, Download, AlertCircle, RefreshCw } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { APP_VERSION } from '../../lib/version';

export default function AboutSection({
  checking,
  upToDate,
  onCheckUpdate,
  updateInfo,
  updateError,
  downloading,
  downloadProgress,
  onDownloadUpdate,
}) {
  const progressPercent =
    downloading && downloadProgress.contentLength > 0
      ? Math.round((downloadProgress.downloaded / downloadProgress.contentLength) * 100)
      : 0;

  const renderUpdateButton = () => {
    if (downloading) {
      return (
        <div className="w-full space-y-1.5">
          <div className="w-full h-2 bg-ide-bg rounded-full overflow-hidden">
            <div
              className="h-full bg-ide-accent rounded-full transition-all duration-300"
              style={{ width: `${progressPercent}%` }}
            />
          </div>
          <div className="text-[10px] text-ide-muted text-center">
            {progressPercent}%
          </div>
        </div>
      );
    }

    if (updateInfo) {
      return (
        <div className="w-full space-y-1.5">
          <div className="text-xs text-ide-accent font-medium text-center">
            v{updateInfo.version} available
          </div>
          <button
            onClick={onDownloadUpdate}
            className="w-full py-1.5 rounded text-xs font-medium transition-all flex items-center justify-center gap-2 bg-ide-accent text-white hover:opacity-90"
          >
            <Download className="w-3 h-3" /> Download & Install
          </button>
        </div>
      );
    }

    if (updateError) {
      return (
        <div className="w-full space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] text-red-400 justify-center">
            <AlertCircle className="w-3 h-3" />
            <span className="truncate max-w-[180px]">{updateError}</span>
          </div>
          <button
            onClick={onCheckUpdate}
            className="w-full py-1.5 rounded text-xs font-medium transition-all flex items-center justify-center gap-2 bg-ide-text text-ide-bg hover:opacity-90"
          >
            <RefreshCw className="w-3 h-3" /> Retry
          </button>
        </div>
      );
    }

    if (upToDate) {
      return (
        <button
          disabled
          className="w-full py-1.5 rounded text-xs font-medium transition-all flex items-center justify-center gap-2 bg-green-500/10 text-green-500 border border-green-500/20 cursor-default"
        >
          <CheckCircle2 className="w-3 h-3" /> Latest
        </button>
      );
    }

    return (
      <button
        onClick={onCheckUpdate}
        disabled={checking}
        className="w-full py-1.5 rounded text-xs font-medium transition-all flex items-center justify-center gap-2 bg-ide-text text-ide-bg hover:opacity-90"
      >
        {checking ? 'Checking...' : 'Check Now'}
      </button>
    );
  };

  return (
    <div className="w-full h-full overflow-y-auto pr-2 text-ide-text select-none custom-scrollbar">
      <div className="flex flex-col gap-6 max-w-2xl mx-auto pb-8 pt-2">
        {/* Header - More Compact */}
        <div className="flex items-center gap-5">
          <div className="relative w-16 h-16 shrink-0 flex items-center justify-center bg-gradient-to-br from-ide-panel to-ide-bg rounded-2xl shadow border border-ide-border group cursor-default">
            <div className="absolute inset-0 bg-ide-accent/5 rounded-2xl transform rotate-3 group-hover:rotate-6 transition-transform duration-500" />
            <svg
              className="w-8 h-8 text-ide-accent relative z-10"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
              <polyline points="14 2 14 8 20 8" />
              <line x1="16" y1="13" x2="8" y2="13" />
              <line x1="16" y1="17" x2="8" y2="17" />
              <polyline points="10 9 9 9 8 9" />
            </svg>
          </div>

          <div className="flex flex-col items-start gap-1">
            <h1 className="text-2xl font-bold text-ide-text tracking-tight">CarbonPaper - 复写纸</h1>
            <div className="flex items-center gap-3">
              <span className="px-2 py-0.5 rounded bg-ide-panel border border-ide-border text-[10px] font-mono text-ide-muted">
                {APP_VERSION}
              </span>
            </div>
          </div>
        </div>

        {/* Content Layout - Single Column */}
        <div className="flex flex-col gap-6">
          <section className="space-y-4">
            {/* Description */}
            <div className="p-4 bg-ide-panel/30 border border-ide-border/50 rounded-xl text-sm leading-relaxed text-ide-muted space-y-4">
              <p>
                This program is under GPL-3.0 Licence.
              </p>
              <p>
                Built by White-NX with ❤️.
              </p>
            </div>

            <div className="bg-ide-panel/50 border border-ide-border rounded-xl p-4 backdrop-blur-sm flex flex-col justify-between">
              <div className="flex items-center justify-between mb-2">
                <h3 className="font-semibold text-ide-text text-sm">Updates</h3>
                <div className="text-[10px] text-ide-muted font-mono">{APP_VERSION}</div>
              </div>
              {renderUpdateButton()}
            </div>

            <div onClick={() => openUrl('https://github.com/White-NX/carbonPaper')} className="block">
              <div className="relative group overflow-hidden bg-gradient-to-br from-indigo-500/10 to-ide-panel border border-indigo-500/20 rounded-xl p-4 cursor-pointer transition-all hover:shadow-lg hover:border-indigo-500/40">
                <div className="relative z-10 flex items-center justify-between">
                  <div>
                    <h2 className="text-sm font-bold text-ide-text group-hover:text-indigo-400 transition-colors">GitHub Repository</h2>
                    <p className="text-xs text-ide-muted">Star, fork, and contribute.</p>
                  </div>
                  <Github className="w-5 h-5 text-ide-muted group-hover:text-indigo-400 transition-colors" />
                </div>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
