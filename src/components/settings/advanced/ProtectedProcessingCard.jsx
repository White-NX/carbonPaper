import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { SettingsDisclosure } from '../SettingsPrimitives';
import { useSettingsActive, useSettingsActivity } from '../SettingsActivityContext';
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
  useEffect(() => {
    if (!active) return undefined;
    refresh();
    const timer = setInterval(refresh, 10000);
    return () => clearInterval(timer);
  }, [refresh, active]);

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
  const stateText = !status ? t('settings.feedback.loading') : needsRepair ? t('appBound.waitingForRepair') : status?.enabled
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
          <button type="button" disabled={busy || !status || (needsRepair ? !canRepair : !status.enabled && !canEnable)}
            className="rounded-lg bg-ide-accent px-3 py-1.5 text-xs text-white disabled:opacity-50"
            onClick={() => run(() => needsRepair ? installAppBound(false) : status.installed
              ? setAppBoundPolicy(!status.enabled, status.enabled ? status.limits.retention_days : Number(days),
                status.enabled ? status.limits.capacity_bytes / 1024 / 1024 : Number(capacity))
              : installAppBound(true))}>
            {busy ? t('appBound.working') : needsRepair ? t('appBound.repair') : status?.enabled
              ? t('appBound.disable') : t('appBound.enable')}
          </button>
        </div>
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
            <button type="button" disabled={busy || !validLimits || needsRepair} className="text-ide-accent disabled:opacity-50"
              onClick={() => run(() => setAppBoundPolicy(status.enabled, Number(days), Number(capacity)))}>
              {t('appBound.saveLimits')}
            </button>
            {!needsRepair && <button type="button" disabled={busy || !canRepair} className="text-ide-accent disabled:opacity-50"
              onClick={() => run(() => installAppBound(false))}>{t('appBound.repair')}</button>}
            <button type="button" disabled={busy} className="text-ide-muted disabled:opacity-50"
              onClick={() => run(uninstallAppBound)}>{t('appBound.uninstall')}</button>
          </div>
        </SettingsDisclosure>}
        {message && <p role="alert" className="text-xs text-amber-600 dark:text-amber-400">{message}</p>}
        {statusError && <p role="alert" className="text-xs text-ide-error">{statusError}</p>}
      </div>
    </section>
  );
}
