import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Info, Loader2, ShieldCheck } from 'lucide-react';
import { getAppBoundStatus, installAppBound, dismissAppBoundOffer, appBoundErrorMessage } from '../lib/app_bound_api';

export default function AppBoundUpgradePrompt({ visible }) {
  const { t } = useTranslation();
  const [offered, setOffered] = useState(false);
  const [debugPreview, setDebugPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [repair, setRepair] = useState(false);
  useEffect(() => {
    if (!visible || debugPreview) return undefined;
    let alive = true;
    getAppBoundStatus().then((status) => {
      if (alive) { setOffered(Boolean(status?.offer_enable)); setRepair(status?.reason === 'repair_required'); }
    }).catch(() => {});
    return () => { alive = false; };
  }, [visible, debugPreview]);
  useEffect(() => {
    const showDebugPreview = (event) => {
      setRepair(Boolean(event.detail?.repair));
      setError('');
      setBusy(false);
      setDebugPreview(true);
    };
    window.addEventListener('debug-show-app-bound-offer', showDebugPreview);
    return () => window.removeEventListener('debug-show-app-bound-offer', showDebugPreview);
  }, []);
  if (!(debugPreview || (visible && offered))) return null;
  const later = async () => {
    if (debugPreview) {
      setDebugPreview(false);
      return;
    }
    try { await dismissAppBoundOffer(); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
  };
  const enable = async () => {
    setBusy(true); setError('');
    try { await installAppBound(!repair); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
    finally { setBusy(false); }
  };
  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div role="dialog" aria-modal="true" aria-labelledby="app-bound-offer-title"
        className="w-full max-w-lg bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center shrink-0">
            <ShieldCheck className="w-5 h-5 text-ide-accent" />
          </div>
          <div>
            <h2 id="app-bound-offer-title" className="text-lg font-semibold text-ide-text">
              {t('appBound.title', '重启后继续后台整理')}
            </h2>
            <p className="text-xs text-ide-muted">
              {t('appBound.subtitle', '重启后无需解锁，整理继续进行')}
            </p>
          </div>
        </div>

        {/* Description */}
        <div className="mt-4 space-y-3">
          <p className="text-sm text-ide-text/90 leading-relaxed">
            {t('appBound.description', '空闲时继续整理新记录，即使重启后尚未解锁。解锁后会自动补全关键词搜索。')}
          </p>

          <div className="flex items-start gap-2 bg-ide-bg rounded-lg border border-ide-border p-3">
            <Info className="w-4 h-4 text-ide-accent shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">
              {t('appBound.installHint', '启用时 Windows 会请求管理员授权，完成后应用会重新启动。')}
            </p>
          </div>
        </div>

        {error && (
          <div role="alert" className="mt-3 text-xs px-3 py-2 rounded bg-red-500/10 text-red-400">
            {error}
          </div>
        )}

        {/* Actions */}
        <div className="mt-5 flex items-center justify-end gap-2">
          <button type="button" disabled={busy}
            className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
            onClick={later}>
            {t('appBound.later', '稍后')}
          </button>
          <button type="button" disabled={busy}
            className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
            onClick={enable}>
            {busy ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <ShieldCheck className="w-3.5 h-3.5" />}
            {busy ? t('appBound.working', '正在处理…') : repair ? t('appBound.repair', '修复组件') : t('appBound.enable', '启用')}
          </button>
        </div>
      </div>
    </div>
  );
}
