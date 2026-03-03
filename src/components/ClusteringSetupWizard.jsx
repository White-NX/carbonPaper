import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Layers, Clock, Loader2, Info } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

/**
 * Clustering Setup Wizard — one-time overlay shown after update for users
 * who have existing screenshots that haven't been clustered into tasks.
 *
 * When the user confirms, the wizard closes immediately and signals the parent
 * to schedule a background clustering run after a 60-second delay (to allow
 * the MiniLM embedding model to finish initialising).
 *
 * Props:
 *   isVisible    — whether the wizard should be rendered
 *   onComplete   — callback: (shouldRun: boolean) => void
 */
export default function ClusteringSetupWizard({ isVisible, onComplete }) {
  const { t } = useTranslation();
  const [applying, setApplying] = useState(false);

  const handleConfirm = async () => {
    setApplying(true);
    try {
      await invoke('mark_clustering_setup_done');
    } catch (err) {
      console.warn('Failed to mark clustering setup done:', err);
    }
    setApplying(false);
    onComplete?.(true);
  };

  const handleSkip = async () => {
    try {
      await invoke('mark_clustering_setup_done');
    } catch (err) {
      console.warn('Failed to mark clustering setup done:', err);
    }
    onComplete?.(false);
  };

  if (!isVisible) return null;

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-lg bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center">
            <Layers className="w-5 h-5 text-ide-accent" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-ide-text">
              {t('clusteringSetup.title')}
            </h2>
            <p className="text-xs text-ide-muted">
              {t('clusteringSetup.subtitle')}
            </p>
          </div>
        </div>

        {/* Description */}
        <div className="mt-4 space-y-3">
          <p className="text-sm text-ide-text/90 leading-relaxed">
            {t('clusteringSetup.description')}
          </p>

          {/* Delay hint */}
          <div className="flex items-start gap-2 bg-ide-bg rounded-lg border border-ide-border p-3">
            <Clock className="w-4 h-4 text-ide-accent shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">
              {t('clusteringSetup.delay_hint')}
            </p>
          </div>

          {/* Info note */}
          <div className="flex items-start gap-2 px-1">
            <Info className="w-3.5 h-3.5 text-ide-muted/60 shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted/70 leading-relaxed">
              {t('clusteringSetup.info_note')}
            </p>
          </div>
        </div>

        {/* Actions */}
        <div className="mt-5 flex items-center justify-end gap-2">
          <button
            onClick={handleSkip}
            disabled={applying}
            className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
          >
            {t('clusteringSetup.skip')}
          </button>
          <button
            onClick={handleConfirm}
            disabled={applying}
            className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
          >
            {applying && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            {t('clusteringSetup.apply')}
          </button>
        </div>
      </div>
    </div>
  );
}
