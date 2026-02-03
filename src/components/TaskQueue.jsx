import React, { useState, useRef, useEffect } from 'react';
import { Loader2, Activity, CheckCircle2, XCircle, Clock, ChevronDown, ChevronRight, Sparkles } from 'lucide-react';
import { cn, getApiBase } from '../lib/utils';

export function TaskQueue({ activeTasks, history, systemStats, onCancel, onCancelBatch, llmState }) {
  const [isLogCollapsed, setIsLogCollapsed] = useState(false);
  const [isPreviewCollapsed, setIsPreviewCollapsed] = useState(false);
  const previewContainerRef = useRef(null);
  const pendingTasks = history.filter((t) => t.status === 'queued' || t.status === 'running');
  const completedTasks = history.filter((t) => t.status === 'completed' || t.status === 'failed').slice(0, 20); // Show last 20

  // Auto-scroll preview timeline
  useEffect(() => {
    if (previewContainerRef.current) {
      previewContainerRef.current.scrollLeft = previewContainerRef.current.scrollWidth;
    }
  }, [activeTasks, pendingTasks]);

  const buildPreviewUrl = (image, cacheBust) => {
    if (!image) return '';
    if (image.dataUrl) return image.dataUrl;
    if (!image.filename) return '';
    const params = new URLSearchParams();
    if (image.filename) params.append('filename', image.filename);
    if (image.subfolder) params.append('subfolder', image.subfolder);
    params.append('type', image.type || 'temp');
    if (cacheBust) params.append('t', cacheBust.toString());
    return `${getApiBase()}/view?${params.toString()}`;
  };

  const formatBytes = (bytes) => {
    if (bytes === undefined || bytes === null) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
  };

  return (
    <div className="flex flex-col h-full">
      {/* System Stats Section */}
      <div className="p-3 border-b border-ide-border bg-ide-bg shrink-0">
        <div className="flex items-center justify-between mb-2">
             <span className="text-xs font-bold uppercase tracking-wide text-ide-muted flex items-center gap-1">
                <Activity className="w-3 h-3" /> System Status
             </span>
             <div className={`w-2 h-2 rounded-full ${systemStats ? 'bg-ide-success' : 'bg-ide-error'}`} title={systemStats ? "Connected" : "Disconnected"} />
        </div>
        {systemStats ? (
          <div className="space-y-3">
            {/* CPU & RAM */}
            {systemStats.system_local && (
              <>
                <div className="space-y-1">
                  <div className="flex justify-between text-[10px] text-ide-muted">
                    <span>CPU Load</span>
                    <span>{systemStats.system_local.cpu_usage}%</span>
                  </div>
                  <div className="h-1 bg-ide-panel rounded-full overflow-hidden">
                    <div 
                      className="h-full bg-blue-500 transition-all duration-500"
                      style={{ width: `${Math.min(100, systemStats.system_local.cpu_usage)}%` }}
                    />
                  </div>
                </div>
                <div className="space-y-1">
                  <div className="flex justify-between text-[10px] text-ide-muted">
                    <span>RAM Usage</span>
                    <span>{formatBytes(systemStats.system_local.ram_used)} / {formatBytes(systemStats.system_local.ram_total)}</span>
                  </div>
                  <div className="h-1 bg-ide-panel rounded-full overflow-hidden">
                    <div 
                      className="h-full bg-green-500 transition-all duration-500"
                      style={{ width: `${(systemStats.system_local.ram_used / systemStats.system_local.ram_total) * 100}%` }}
                    />
                  </div>
                </div>
              </>
            )}

            {/* GPU VRAM */}
            {systemStats.devices && systemStats.devices.map((device, idx) => {
                const used = device.vram_total - device.vram_free;
                const percent = (used / device.vram_total) * 100;
                return (
                    <div key={idx} className="space-y-1">
                        <div className="flex justify-between text-[10px] text-ide-muted">
                            <span className="truncate max-w-[120px]" title={device.name}>{device.name} (VRAM)</span>
                            <span>{formatBytes(used)} / {formatBytes(device.vram_total)}</span>
                        </div>
                        <div className="h-1 bg-ide-panel rounded-full overflow-hidden">
                            <div 
                                className="h-full bg-purple-500 transition-all duration-500"
                                style={{ width: `${percent}%` }}
                            />
                        </div>
                    </div>
                );
            })}
          </div>
        ) : (
            <div className="text-[10px] text-ide-muted italic">
                Connecting to ComfyUI...
            </div>
        )}
      </div>

      {/* LLM Status Section */}
          {llmState && (llmState.isThinking || llmState.output) && (
          <div className="p-3 border-b border-ide-border bg-ide-bg shrink-0">
              <div className="flex items-center justify-between mb-2">
                  <span className="text-xs font-bold uppercase tracking-wide text-ide-accent flex items-center gap-1">
                      <Sparkles className="w-3 h-3 animate-pulse" /> 
                      AI Optimizing Prompt
                  </span>
              </div>
              <div className="bg-ide-panel rounded p-2 text-xs font-mono text-ide-text/80 max-h-32 overflow-y-auto whitespace-pre-wrap">
                {llmState.output || "Thinking..."}
              </div>
          </div>
      )}

      {/* Active Tasks Section */}
      <div className={`p-3 border-b border-ide-border ${isLogCollapsed ? 'flex-1 overflow-y-auto' : 'shrink-0'}`}>
        <div className="flex items-center justify-between mb-2">
          <span className="text-xs font-bold uppercase tracking-wide text-ide-muted">Active Processes</span>
          <div className="flex items-center gap-2">
            {pendingTasks.length > 1 && onCancelBatch && (
              <button 
                onClick={onCancelBatch}
                className="text-[10px] text-ide-muted hover:text-ide-error uppercase font-bold transition-colors"
                title="Cancel all pending tasks except the current one"
              >
                Clear Queue
              </button>
            )}
            {pendingTasks.length > 0 && <Loader2 className="w-3 h-3 animate-spin text-ide-accent" />}
          </div>
        </div>
        
        {pendingTasks.length === 0 ? (
          <div className="text-xs text-ide-muted italic py-2">No active processes</div>
        ) : (
          <div className="space-y-2">
            {pendingTasks.map((task) => {
              const progress = activeTasks[task.promptId]?.progress;
              const total = progress?.max || 1;
              const value = progress?.value || 0;
              const percent = Math.min(100, Math.max(0, Math.round((value / total) * 100)));

              return (
                <div key={task.promptId} className="bg-ide-bg border border-ide-border p-2 rounded relative group">
                  <div className="flex justify-between items-start mb-1 pr-4">
                    <span className="text-xs font-mono text-ide-text truncate w-2/3" title={task.prompt}>
                      {task.prompt}
                    </span>
                    <span className="text-[10px] font-mono text-ide-muted">
                      {percent}%
                    </span>
                  </div>

                  {onCancel && (
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onCancel(task.promptId);
                      }}
                      className="absolute top-2 right-2 text-ide-muted hover:text-ide-error opacity-0 group-hover:opacity-100 transition-opacity"
                      title="Cancel task"
                    >
                      <XCircle className="w-3 h-3" />
                    </button>
                  )}

                  <div className="h-1 bg-ide-panel rounded-full overflow-hidden">
                    <div 
                      className="h-full bg-ide-accent transition-all duration-300"
                      style={{ width: `${percent}%` }}
                    />
                  </div>
                  <div className="flex justify-between mt-1">
                    <span className="text-[10px] text-ide-muted uppercase">{task.status}</span>
                    <span className="text-[10px] font-mono text-ide-muted">#{task.promptId.slice(0, 6)}</span>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Preview Section (vertical timeline, collapsible) */}
      <div className="p-3 border-b border-ide-border bg-ide-bg">
        <div 
          className="flex items-center justify-between mb-2 cursor-pointer select-none group"
          onClick={() => setIsPreviewCollapsed(!isPreviewCollapsed)}
        >
          <div className="flex items-center gap-2">
            {isPreviewCollapsed ? (
              <ChevronRight className="w-4 h-4 text-ide-muted group-hover:text-ide-text transition-colors" />
            ) : (
              <ChevronDown className="w-4 h-4 text-ide-muted group-hover:text-ide-text transition-colors" />
            )}
            <span className="text-xs font-bold uppercase tracking-wide text-ide-muted group-hover:text-ide-text transition-colors">Preview Timeline</span>
          </div>
          <span className="text-[10px] font-mono text-ide-muted">
            {(() => {
                if (pendingTasks.length === 0) return 'Idle';
                const runningTask = pendingTasks.find(t => t.status === 'running');
                const currentTask = runningTask || pendingTasks[pendingTasks.length - 1];
                return `#${currentTask.promptId.slice(0, 6)}`;
            })()}
          </span>
        </div>

        {!isPreviewCollapsed && (
          <div 
            ref={previewContainerRef}
            className="overflow-x-auto pb-2"
          >
            {pendingTasks.length === 0 ? (
              <div className="text-xs text-ide-muted italic">No active tasks</div>
            ) : (() => {
              // Prioritize running task, otherwise show the one at the front of the queue (oldest)
              // history is Newest -> Oldest, so pendingTasks is also Newest -> Oldest.
              // The task being processed is likely the last one in the list (Oldest).
              const runningTask = pendingTasks.find(t => t.status === 'running');
              const currentTask = runningTask || pendingTasks[pendingTasks.length - 1];
              
              const previews = (activeTasks[currentTask.promptId]?.previews || []).slice(-50);
              if (previews.length === 0) {
                return <div className="text-xs text-ide-muted italic">Waiting for preview... (#{currentTask.promptId.slice(0, 6)})</div>;
              }

              return (
                <div className="flex gap-2 min-w-max">
                  {previews.map((preview, idx) => {
                    const isLatest = idx === previews.length - 1;
                    const stepLabel = (() => {
                      const stepValue = typeof preview.step === 'number' ? Math.round(preview.step) : null;
                      const maxValue = typeof preview.max === 'number' ? Math.round(preview.max) : null;
                      if (stepValue !== null && maxValue !== null) return `${stepValue}/${maxValue}`;
                      if (stepValue !== null) return `Step ${stepValue}`;
                      return 'Preview';
                    })();

                    return (
                      <div key={`${preview.image?.filename || preview.image?.dataUrl || 'preview'}-${preview.receivedAt || idx}`} className="flex flex-col items-center gap-1">
                        <div className={cn('relative border rounded overflow-hidden bg-ide-panel shrink-0', isLatest ? 'border-ide-accent/80' : 'border-ide-border')}>
                          <img
                            src={buildPreviewUrl(preview.image, preview.receivedAt)}
                            alt="Preview"
                            className="w-24 h-32 object-cover"
                          />
                          <div className="absolute inset-x-0 bottom-0 bg-black/60 text-[9px] leading-tight text-white text-center px-1 py-[1px]">
                            {stepLabel}
                          </div>
                        </div>
                        <div className={cn('w-1.5 h-1.5 rounded-full', isLatest ? 'bg-ide-accent' : 'bg-ide-muted')} />
                      </div>
                    );
                  })}
                </div>
              );
            })()}
          </div>
        )}
      </div>

      {/* Log Section */}
      <div className={`${isLogCollapsed ? 'flex-none' : 'flex-1'} overflow-y-auto p-3 flex flex-col min-h-[40px]`}>
        <div 
          className="flex items-center justify-between mb-2 cursor-pointer select-none group shrink-0"
          onClick={() => setIsLogCollapsed(!isLogCollapsed)}
        >
          <div className="flex items-center gap-2">
            {isLogCollapsed ? (
              <ChevronRight className="w-4 h-4 text-ide-muted group-hover:text-ide-text transition-colors" />
            ) : (
              <ChevronDown className="w-4 h-4 text-ide-muted group-hover:text-ide-text transition-colors" />
            )}
            <span className="text-xs font-bold uppercase tracking-wide text-ide-muted group-hover:text-ide-text transition-colors">Output Log</span>
          </div>
        </div>
        {!isLogCollapsed && (
          <div className="space-y-1 font-mono text-[11px] flex-1">
            {completedTasks.map((task) => (
              <div key={task.promptId} className="flex gap-2 items-start hover:bg-ide-hover p-1 rounded cursor-default group">
                <div className="mt-0.5">
                  {task.status === 'completed' ? (
                    <CheckCircle2 className="w-3 h-3 text-ide-success" />
                  ) : task.status === 'failed' ? (
                    <XCircle className="w-3 h-3 text-ide-error" />
                  ) : (
                    <Clock className="w-3 h-3 text-ide-warning" />
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                        <span className={cn(
                        "font-bold",
                        task.status === 'completed' ? "text-ide-success" : "text-ide-error"
                        )}>
                        [{task.status.toUpperCase()}]
                        </span>
                        <span className="text-ide-muted">
                        {new Date(task.completedAt || task.createdAt || Date.now()).toLocaleTimeString()}
                        </span>
                        {task.duration && (
                        <span className="text-[10px] text-ide-muted ml-2">
                            ({task.duration}s)
                        </span>
                        )}
                    </div>
                    {onCancel && (
                        <button
                            onClick={(e) => {
                                e.stopPropagation();
                                onCancel(task.promptId);
                            }}
                            className="text-ide-muted hover:text-ide-error opacity-0 group-hover:opacity-100 transition-opacity"
                            title="Delete log entry"
                        >
                            <XCircle className="w-3 h-3" />
                        </button>
                    )}
                  </div>
                  <div className="text-ide-text truncate" title={task.prompt}>
                    {task.prompt}
                  </div>
                  <div className="text-ide-muted text-[10px]">
                    ID: {task.promptId}
                  </div>
                </div>
              </div>
            ))}
            {completedTasks.length === 0 && (
              <div className="text-ide-muted italic">No logs available</div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
