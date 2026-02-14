import React from 'react';
import { Github, User, CheckCircle2, RefreshCw } from 'lucide-react';
import { APP_VERSION } from '../../lib/version';

export default function AboutSection({ checking, upToDate, onCheckUpdate }) {
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
              <h3 className="font-semibold text-ide-text text-base">关于项目</h3>
              <p>
                复写纸（carbonpaper）是一款开源的屏幕文字捕捉与智能检索工具，旨在帮助用户高效地记录和查找屏幕上的文字内容。
                通过集成本地的OCR技术和语义搜索算法，复写纸能够实时捕捉屏幕文字，并将其转换为可搜索的文本数据。
              </p>
            </div>
            
            {/* Updates, Contributors & Github Link - Stacked or Grid inside column */}
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
               <div className="bg-ide-panel/50 border border-ide-border rounded-xl p-4 backdrop-blur-sm flex flex-col justify-between">
                 <div className="flex items-center justify-between mb-2">
                    <h3 className="font-semibold text-ide-text text-sm">Updates</h3>
                    <div className="text-[10px] text-ide-muted font-mono">{APP_VERSION}</div>
                 </div>
                 <button 
                  onClick={onCheckUpdate}
                  disabled={checking || upToDate}
                  className={`w-full py-1.5 rounded text-xs font-medium transition-all flex items-center justify-center gap-2 ${
                    upToDate
                      ? 'bg-green-500/10 text-green-500 border border-green-500/20 cursor-default'
                      : 'bg-ide-text text-ide-bg hover:opacity-90'
                  }`}
                >
                  {checking ? 'Checking...' : upToDate ? <><CheckCircle2 className="w-3 h-3" /> Latest</> : 'Check Now'}
                </button>
               </div>

               <div className="bg-ide-panel/50 border border-ide-border rounded-xl p-4 backdrop-blur-sm flex flex-col justify-between">
                <div className="flex items-center gap-2 mb-2">
                  <h3 className="font-semibold text-ide-text text-sm">Contributors</h3>
                </div>
                <div className="flex items-center justify-between">
                   <div className="flex -space-x-2">
                    {[1, 2, 3].map((i) => (
                      <div
                        key={i}
                        className="w-6 h-6 rounded-full bg-ide-bg border border-ide-border flex items-center justify-center text-ide-muted text-[10px]"
                      >
                        <User className="w-3 h-3" />
                      </div>
                    ))}
                  </div>
                   <button className="px-2 py-1 rounded-full bg-ide-bg border border-dashed border-ide-border text-[10px] text-ide-muted hover:text-ide-text hover:border-ide-accent hover:bg-ide-accent/5 transition-all">
                    + Join
                  </button>
                </div>
              </div>
            </div>

            <a href="https://github.com/White-NX/carbonPaper" target="_blank" rel="noreferrer" className="block">
              <div className="relative group overflow-hidden bg-gradient-to-br from-indigo-500/10 to-ide-panel border border-indigo-500/20 rounded-xl p-4 cursor-pointer transition-all hover:shadow-lg hover:border-indigo-500/40">
                <div className="relative z-10 flex items-center justify-between">
                  <div>
                    <h2 className="text-sm font-bold text-ide-text group-hover:text-indigo-400 transition-colors">GitHub Repository</h2>
                    <p className="text-xs text-ide-muted">Star, fork, and contribute.</p>
                  </div>
                  <Github className="w-5 h-5 text-ide-muted group-hover:text-indigo-400 transition-colors" />
                </div>
              </div>
            </a>
          </section>
        </div>
      </div>
    </div>
  );
}
