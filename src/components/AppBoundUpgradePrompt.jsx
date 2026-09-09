import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getAppBoundStatus, installAppBound, dismissAppBoundOffer, appBoundErrorMessage } from '../lib/app_bound_api';

export default function AppBoundUpgradePrompt({ visible }) {
  const { t } = useTranslation();
  const [offered, setOffered] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [repair, setRepair] = useState(false);
  useEffect(() => {
    if (!visible) return undefined;
    let alive = true;
    getAppBoundStatus().then((status) => {
      if (alive) { setOffered(Boolean(status?.offer_enable)); setRepair(status?.reason === 'repair_required'); }
    }).catch(() => {});
    return () => { alive = false; };
  }, [visible]);
  if (!visible || !offered) return null;
  const later = async () => {
    try { await dismissAppBoundOffer(); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
  };
  const enable = async () => {
    setBusy(true); setError('');
    try { await installAppBound(!repair); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
    finally { setBusy(false); }
  };
  return (
    <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/40 p-6">
      <div role="dialog" aria-modal="true" aria-labelledby="app-bound-offer-title"
        className="w-full max-w-md space-y-4 rounded-xl border border-ide-border bg-ide-bg p-6 shadow-xl">
        <h2 id="app-bound-offer-title" className="text-lg font-semibold text-ide-text">{t('appBound.title', '重启后继续后台整理')}</h2>
        <p className="text-sm leading-relaxed text-ide-muted">{t('appBound.description', '空闲时继续整理新记录，即使重启后尚未解锁。解锁后会自动补全关键词搜索。')}</p>
        <p className="text-xs text-ide-muted">{t('appBound.installHint', '启用时 Windows 会请求管理员授权，完成后应用会重新启动。')}</p>
        {error && <p role="alert" className="text-sm text-amber-600">{error}</p>}
        <div className="flex justify-end gap-3">
          <button type="button" disabled={busy} className="rounded-lg border border-ide-border px-4 py-2 text-sm text-ide-text disabled:opacity-50" onClick={later}>
            {t('appBound.later', '稍后')}
          </button>
          <button type="button" disabled={busy} className="rounded-lg bg-ide-accent px-4 py-2 text-sm text-white disabled:opacity-50" onClick={enable}>
            {busy ? t('appBound.working', '正在处理…') : repair ? t('appBound.repair', '修复组件') : t('appBound.enable', '启用')}
          </button>
        </div>
      </div>
    </div>
  );
}
