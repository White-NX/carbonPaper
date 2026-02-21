import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Zap, Monitor, Gamepad2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

/**
 * Classify a GPU name as integrated or discrete.
 */
function classifyGpu(name) {
  if (/intel\s*(uhd|iris|hd)\s*/i.test(name)) return 'integrated';
  if (/radeon(?!.*\b(rx|pro|vii|xt)\b)/i.test(name)) return 'integrated';
  return 'discrete';
}

/**
 * DML Setup Wizard — one-time overlay shown on first launch after update.
 * Detects GPUs and recommends DirectML + Game Mode configuration.
 */
export default function DmlSetupWizard({ isVisible, onComplete }) {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [gpus, setGpus] = useState([]);
  const [selectedGpuId, setSelectedGpuId] = useState(0);
  const [enableDml, setEnableDml] = useState(true);
  const [enableGameMode, setEnableGameMode] = useState(true);
  const [applying, setApplying] = useState(false);

  useEffect(() => {
    if (!isVisible) return;
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke('enumerate_gpus');
        if (cancelled) return;
        const classified = (list || []).map((g) => ({
          ...g,
          type: classifyGpu(g.name),
        }));
        setGpus(classified);

        if (classified.length === 0) {
          setEnableDml(false);
          setEnableGameMode(false);
        } else if (classified.length === 1) {
          setSelectedGpuId(classified[0].id);
          setEnableDml(true);
          setEnableGameMode(true);
        } else {
          // prefer integrated GPU; disable game mode when integrated is selected
          const integrated = classified.find((g) => g.type === 'integrated');
          setSelectedGpuId(integrated ? integrated.id : classified[0].id);
          setEnableDml(true);
          setEnableGameMode(!integrated);
        }
      } catch (err) {
        console.error('Failed to enumerate GPUs:', err);
        if (!cancelled) {
          setGpus([]);
          setEnableDml(false);
          setEnableGameMode(false);
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [isVisible]);

  const handleApply = async () => {
    setApplying(true);
    try {
      if (gpus.length > 0) {
        await invoke('set_advanced_config', {
          config: {
            use_dml: enableDml,
            dml_device_id: selectedGpuId,
          },
        });
        await invoke('toggle_game_mode', { enabled: enableGameMode && enableDml });
      }
      await invoke('mark_dml_setup_done');
      onComplete?.();
    } catch (err) {
      console.error('Failed to apply DML setup:', err);
      // still mark as done so user isn't stuck
      try { await invoke('mark_dml_setup_done'); } catch {}
      onComplete?.();
    } finally {
      setApplying(false);
    }
  };

  const handleSkip = async () => {
    try { await invoke('mark_dml_setup_done'); } catch {}
    onComplete?.();
  };

  if (!isVisible) return null;

  const integratedGpu = gpus.find((g) => g.type === 'integrated');
  const hasMultipleGpus = gpus.length >= 2;
  const hasSingleGpu = gpus.length === 1;
  const hasNoGpu = gpus.length === 0;
  const selectedGpu = gpus.find((g) => g.id === selectedGpuId);
  const selectedIsIntegrated = hasMultipleGpus && selectedGpu?.type === 'integrated';

  const handleGpuSelect = (gpuId) => {
    if (!hasMultipleGpus) return;
    setSelectedGpuId(gpuId);
    const gpu = gpus.find((g) => g.id === gpuId);
    if (gpu?.type === 'integrated') {
      setEnableGameMode(false);
    } else {
      setEnableGameMode(true);
    }
  };

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-lg bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center">
            <Zap className="w-5 h-5 text-ide-accent" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-ide-text">{t('dmlSetup.title')}</h2>
            <p className="text-xs text-ide-muted">{t('dmlSetup.subtitle')}</p>
          </div>
        </div>

        {loading ? (
          <div className="flex items-center justify-center gap-2 py-10 text-sm text-ide-muted">
            <Loader2 className="w-4 h-4 animate-spin" />
            <span>{t('dmlSetup.loading')}</span>
          </div>
        ) : (
          <>
            {/* GPU list */}
            {hasNoGpu ? (
              <div className="mt-4 p-3 bg-ide-bg rounded-lg border border-ide-border text-sm">
                <p className="text-ide-muted">{t('dmlSetup.no_gpu')}</p>
                <p className="text-ide-muted/70 text-xs mt-1">{t('dmlSetup.no_gpu_note')}</p>
              </div>
            ) : (
              <>
                <p className="text-xs text-ide-muted mt-4 mb-2">{t('dmlSetup.detected_gpus')}</p>
                <div className="bg-ide-bg rounded-lg border border-ide-border overflow-hidden">
                  {gpus.map((gpu) => {
                    const isSelected = selectedGpuId === gpu.id;
                    const isRecommended = hasMultipleGpus && gpu.type === 'integrated';
                    const tagLabel = gpu.type === 'integrated'
                      ? t('dmlSetup.tag_integrated')
                      : t('dmlSetup.tag_discrete');
                    const tagColor = gpu.type === 'integrated'
                      ? 'bg-blue-500/15 text-blue-400 border-blue-500/30'
                      : 'bg-orange-500/15 text-orange-400 border-orange-500/30';

                    return (
                      <label
                        key={gpu.id}
                        className={`flex items-center gap-3 px-3 py-2.5 cursor-pointer transition-colors hover:bg-ide-bg/50 ${
                          isSelected ? 'bg-ide-accent/10' : ''
                        } ${gpu.id > 0 ? 'border-t border-ide-border' : ''}`}
                        onClick={() => handleGpuSelect(gpu.id)}
                      >
                        {hasMultipleGpus && (
                          <input
                            type="radio"
                            name="gpu-select"
                            checked={isSelected}
                            onChange={() => handleGpuSelect(gpu.id)}
                            className="accent-ide-accent"
                          />
                        )}
                        {hasSingleGpu && (
                          <Monitor className="w-4 h-4 text-ide-muted/70 shrink-0" />
                        )}
                        <span className="text-sm text-ide-text flex-1">
                          GPU {gpu.id}: {gpu.name}
                        </span>
                        <span className={`text-[10px] px-1.5 py-0.5 rounded border ${tagColor}`}>
                          {tagLabel}
                        </span>
                        {isRecommended && (
                          <span className="text-[10px] px-1.5 py-0.5 rounded border bg-green-500/15 text-green-400 border-green-500/30">
                            {t('dmlSetup.tag_recommended')}
                          </span>
                        )}
                      </label>
                    );
                  })}
                </div>

                {/* Recommendation text */}
                <p className="text-xs text-ide-muted mt-3">
                  {hasMultipleGpus ? t('dmlSetup.multi_gpu_recommend') : t('dmlSetup.single_gpu_recommend')}
                </p>

                {/* Checkboxes */}
                <div className="mt-4 space-y-2">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={enableDml}
                      onChange={(e) => setEnableDml(e.target.checked)}
                      className="accent-ide-accent"
                    />
                    <Zap className="w-3.5 h-3.5 text-ide-muted/70" />
                    <span className="text-sm text-ide-text">{t('dmlSetup.enable_dml')}</span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={enableGameMode}
                      onChange={(e) => setEnableGameMode(e.target.checked)}
                      className="accent-ide-accent"
                    />
                    <Gamepad2 className="w-3.5 h-3.5 text-ide-muted/70" />
                    <span className="text-sm text-ide-text">{t('dmlSetup.enable_game_mode')}</span>
                    <span className="text-[10px] text-ide-muted/60 ml-1">
                      {t('dmlSetup.game_mode_hint')}
                    </span>
                  </label>
                  {selectedIsIntegrated && (
                    <p className="text-[11px] text-yellow-400/80 ml-6 leading-snug">
                      {t('dmlSetup.game_mode_multi_gpu_note')}
                    </p>
                  )}
                </div>
              </>
            )}

            {/* Actions */}
            <div className="mt-5 flex items-center justify-end gap-2">
              {hasNoGpu ? (
                <button
                  onClick={handleSkip}
                  className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors"
                >
                  {t('dmlSetup.acknowledge')}
                </button>
              ) : (
                <>
                  <button
                    onClick={handleSkip}
                    disabled={applying}
                    className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
                  >
                    {t('dmlSetup.skip')}
                  </button>
                  <button
                    onClick={handleApply}
                    disabled={applying}
                    className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
                  >
                    {applying && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
                    {t('dmlSetup.apply')}
                  </button>
                </>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
