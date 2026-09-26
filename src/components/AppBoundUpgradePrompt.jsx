import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { getAppBoundStatus, installAppBound, dismissAppBoundOffer, appBoundErrorMessage } from '../lib/app_bound_api';
import { OverlayShell, useDebugOverlay, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

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
  useDebugOverlay('appBound', (variant) => {
    setRepair(variant === 'repair');
    setError('');
    setBusy(false);
    setDebugPreview(true);
  });
  const shown = useOverlaySlot('appBound', debugPreview || (visible && offered));
  if (!shown) return null;
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
    <OverlayShell
      icon={ShieldCheck}
      title={t('appBound.title', '重启后继续后台整理')}
      subtitle={t('appBound.subtitle', '重启后无需解锁，整理继续进行')}
      footer={(
        <>
          <Button size="md" disabled={busy} onClick={later}>
            {t('appBound.later', '稍后')}
          </Button>
          <Button size="md" variant="primary" icon={ShieldCheck} loading={busy} onClick={enable}>
            {busy ? t('appBound.working', '正在处理…') : repair ? t('appBound.repair', '修复组件') : t('appBound.enable', '启用')}
          </Button>
        </>
      )}
    >
      <p className="text-sm leading-relaxed text-ide-text/90">
        {t('appBound.description', '空闲时继续整理新记录，即使重启后尚未解锁。解锁后会自动补全关键词搜索。')}
      </p>
      <Banner tone="info">
        {t('appBound.installHint', '启用时 Windows 会请求管理员授权，完成后应用会重新启动。')}
      </Banner>
      {error && <Banner tone="error">{error}</Banner>}
    </OverlayShell>
  );
}
