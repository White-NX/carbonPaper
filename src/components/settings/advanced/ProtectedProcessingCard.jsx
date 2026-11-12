import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { SettingsDisclosure } from '../SettingsPrimitives';
import { useSettingsActive, useSettingsActivity } from '../SettingsActivityContext';
import { Button } from '../../ui/Button';
import { Banner } from '../../ui/Banner';
import { usePolling } from '../../../hooks/usePolling';
import {
  getAppBoundStatus, installAppBound, setAppBoundPolicy, uninstallAppBound, appBoundErrorMessage,
} from '../../../lib/app_bound_api';

export default function ProtectedProcessingCard({ backgroundEnabled = true }) {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [statusError, setStatusError] = useState('');
  const [days, setDays] = useState(30);
  const [capacity, setCapacity] = useState(4096);
  const dirty = useRef(false);
  useSettingsActivity('protected-processing', { dirty: dirty.current, busy });
  const mounted = useRef(false);
  const refresh = useCallback(async () => {
    try {
      const next = await getAppBoundStatus();
      if (!mounted.current) return;
      setStatus(next);
      setStatusError('');
      if (!dirty.current && next?.limits) {
        setDays(next.limits.retention_days);
        setCapacity(next.limits.capacity_bytes / 1024 / 1024);
      }
    } catch {
      if (mounted.current) setStatusError(t('settings.feedback.readFailed'));
    }
  }, [t]);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  usePolling(refresh, { intervalMs: 10000, enabled: active });

  const run = async (action) => {
    setBusy(true);
    setMessage('');
    try {
      await action();
      dirty.current = false;
      await refresh();
    } catch (error) {
      if (mounted.current) setMessage(appBoundErrorMessage(error, t));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  const validLimits = Number.isInteger(Number(days)) && Number(days) >= 1 && Number(days) <= 30
    && Number.isInteger(Number(capacity)) && Number(capacity) >= 256 && Number(capacity) <= 4096;
  const canEnable = backgroundEnabled && status?.supported && (status?.installed || status?.package_available) && validLimits;
  const canRepair = status?.supported && status?.package_available;
  const needsRepair = status?.reason === 'repair_required';
  const recovering = status?.reason === 'service_unavailable';
  const needsUpgrade = status?.reason === 'upgrade_required';
  const needsMaintenance = needsRepair || needsUpgrade || recovering;
  const stateText = !status ? t('settings.feedback.loading') : needsUpgrade ? t('appBound.waitingForUpgrade')
    : recovering ? t('appBound.waitingForRecovery')
    : needsRepair ? t('appBound.waitingForRepair') : status?.enabled
    ? status.available
      ? t('appBound.active')
      : t('appBound.waitingForRepair')
    : t('appBound.inactive');

  return (
    <section className="space-y-3" aria-label={t('appBound.title')}>
      <h3 className="flex items-center gap-2 px-1 text-sm font-semibold text-ide-accent">
        <ShieldCheck size={16} />{t('appBound.title')}
      </h3>
      <div className="space-y-4 rounded-xl border border-ide-border bg-ide-bg p-4">
        <p className="text-xs leading-relaxed text-ide-muted">
          {t('appBound.description')}
        </p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span className="text-sm text-ide-text" role="status">{stateText}</span>
          <Button variant="primary" loading={busy}
            disabled={!status || (needsMaintenance ? !canRepair : !status.enabled && !canEnable)}
            onClick={() => run(() => needsMaintenance ? installAppBound(false) : status.installed
              ? setAppBoundPolicy(!status.enabled, status.enabled ? status.limits.retention_days : Number(days),
                status.enabled ? status.limits.capacity_bytes / 1024 / 1024 : Number(capacity))
              : installAppBound(true))}>
            {busy ? t('appBound.working') : needsUpgrade ? t('appBound.upgrade') : needsRepair || recovering ? t('appBound.repair') : status?.enabled
              ? t('appBound.disable') : t('appBound.enable')}
          </Button>
        </div>
        {needsUpgrade && <Banner tone="info">{t('appBound.upgradeHint')}</Banner>}
        {recovering && <Banner tone="info">{t('appBound.recoveryHint')}</Banner>}
        {!status?.installed && <p className="text-xs text-ide-muted">
          {t('appBound.installHint')}
        </p>}
        {status?.supported === false && <p className="text-xs text-ide-muted">
          {t('appBound.packageUnavailable')}
        </p>}
        {status?.installed && <SettingsDisclosure title={t('settings.details.options')}>
          <div className="grid grid-cols-2 gap-3">
            <label className="space-y-1 text-xs text-ide-muted">
              <span>{t('appBound.retention')}</span>
              <input type="number" min="1" max="30" step="1" value={days} disabled={busy}
                className="w-full rounded border border-ide-border bg-ide-panel p-2 text-ide-text"
                onChange={(event) => { dirty.current = true; setDays(event.target.value); }} />
            </label>
            <label className="space-y-1 text-xs text-ide-muted">
              <span>{t('appBound.capacity')}</span>
              <input type="number" min="256" max="4096" step="256" value={capacity} disabled={busy}
                className="w-full rounded border border-ide-border bg-ide-panel p-2 text-ide-text"
                onChange={(event) => { dirty.current = true; setCapacity(event.target.value); }} />
            </label>
          </div>
          <p className="text-xs text-ide-muted">{t('appBound.limitHint')}</p>
          <div className="flex flex-wrap gap-3 text-xs">
            <Button variant="ghost" size="xs" disabled={busy || !validLimits || needsMaintenance}
              onClick={() => run(() => setAppBoundPolicy(status.enabled, Number(days), Number(capacity)))}>
              {t('appBound.saveLimits')}
            </Button>
            {!needsMaintenance && <Button variant="ghost" size="xs" disabled={busy || !canRepair}
              onClick={() => run(() => installAppBound(false))}>{t('appBound.repair')}</Button>}
            <Button variant="ghost" size="xs" disabled={busy}
              onClick={() => run(uninstallAppBound)}>{t('appBound.uninstall')}</Button>
          </div>
        </SettingsDisclosure>}
        {message && <Banner tone="error">{message}</Banner>}
        {statusError && <Banner tone="error">{statusError}</Banner>}
      </div>
    </section>
  );
}
