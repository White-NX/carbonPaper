import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { formatError } from '../../lib/errors';
import { notifySettingsChanged, saveAdvancedConfig } from '../../lib/settings_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';
import { RERANKER_MODEL_ID } from './onboardingPlan';

const PENDING_KEY = 'onboarding.pendingSmartCluster';

function readPending() {
  try { return localStorage.getItem(PENDING_KEY) === '1'; } catch { return false; }
}

function writePending(pending) {
  try {
    if (pending) localStorage.setItem(PENDING_KEY, '1');
    else localStorage.removeItem(PENDING_KEY);
  } catch {
    // Without storage the download still runs this session; it just will not
    // resume on its own after a restart.
  }
}

/**
 * Downloads the smart-cluster component in the background and switches the
 * feature on when it lands.
 *
 * The request outlives the wizard and the process: turning on background
 * processing restarts the app, and a user may quit mid-download. The intent
 * is kept in localStorage and picked up on the next launch; aria2 resumes the
 * partial file. The download waits for the required models so the two never
 * share the install log.
 */
export function useRerankerDownload({ ready, notify, t }) {
  const [status, setStatus] = useState(() => (readPending() ? 'queued' : 'idle'));
  const [percent, setPercent] = useState(null);
  const [error, setError] = useState('');
  const startedRef = useRef(false);

  useTauriEventListener('install-log', (event) => {
    const payload = event?.payload || {};
    if (payload.source !== 'aria2' || !String(payload.file || '').endsWith('.onnx')) return;
    const match = String(payload.line || '').match(/\((\d+)%\)/);
    if (match) setPercent(Number(match[1]));
  }, [], status === 'downloading');

  const run = useCallback(async () => {
    if (startedRef.current) return;
    startedRef.current = true;
    setStatus('downloading');
    setPercent(null);
    setError('');
    try {
      await withAuth(() => invoke('download_model', { modelId: RERANKER_MODEL_ID }), { autoPrompt: true });
      await saveAdvancedConfig({ smart_cluster_enabled: true });
      writePending(false);
      setStatus('done');
      notifySettingsChanged(['models', 'advanced']);
      notify?.({ type: 'success', title: t('onboarding.smartClusterReady.title'), message: t('onboarding.smartClusterReady.message') });
    } catch (failure) {
      const message = formatError(failure);
      setError(message);
      setStatus('failed');
      notify?.({ type: 'error', title: t('onboarding.smartClusterFailed.title'), message: t('onboarding.smartClusterFailed.message', { error: message }) });
    } finally {
      startedRef.current = false;
    }
  }, [notify, t]);

  useEffect(() => {
    if (status === 'queued' && ready) run();
  }, [status, ready, run]);

  /** Remember the request and start as soon as the required models are in. */
  const queue = useCallback(() => {
    writePending(true);
    setStatus((current) => (current === 'downloading' || current === 'done' ? current : 'queued'));
  }, []);

  const retry = useCallback(() => {
    setError('');
    setStatus('queued');
  }, []);

  return { status, percent, error, queue, retry };
}
