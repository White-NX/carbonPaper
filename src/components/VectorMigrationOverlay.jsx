import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Database, Image as ImageIcon, KeyRound, Loader2, ShieldAlert, Wrench } from 'lucide-react';
import {
  getClipRebuildStatus,
  getMaintenanceStatus,
  getMinilmRebuildStatus,
} from '../lib/task_api';
import { requestAuth } from '../lib/auth_api';
import { cn } from '../lib/utils';

const ACTIVE_POLL_MS = 1000;
const IDLE_POLL_MS = 3000;
// Phases whose progress pair drives the bar and the ETA estimate. Shared by
// both migrations: they run the same orchestration out of `migration_support`
// and therefore report the same phase names.
const PROGRESS_SOURCES = {
  copying_chroma: ['chroma_processed', 'chroma_total'],
  publishing_write: ['publish_current', 'publish_total'],
  publishing_sync: ['publish_current', 'publish_total'],
  publishing_verify: ['publish_current', 'publish_total'],
};

/**
 * Which detailed status to read, keyed by the reason string the backend passes
 * to `maintenance::enter`. Keeping the two in step is what stops this overlay
 * from going blank the next time a migration is added: an unrecognised reason
 * still renders a box, just without progress.
 */
const MIGRATION_KINDS = {
  minilm_migration: { id: 'minilm', icon: Database, read: getMinilmRebuildStatus },
  clip_migration: { id: 'clip', icon: ImageIcon, read: getClipRebuildStatus },
};

function formatEta(seconds) {
  if (!Number.isFinite(seconds) || seconds <= 0) return null;
  if (seconds < 60) return `< 1 min`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `≈ ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return `≈ ${hours} h ${minutes % 60} min`;
}

/**
 * Full-window, non-dismissable maintenance overlay for the sentinel-triggered
 * vector migrations. Neither run can be cancelled: closing the app merely
 * interrupts it, and it resumes on the next launch/unlock.
 *
 * Visibility is decided by *maintenance mode*, not by any one migration's
 * `running` flag. The guard is taken before a run marks itself running and
 * dropped after it clears that flag, so maintenance strictly contains both
 * runs — and gating on the outer condition means the app can no longer sit in
 * maintenance mode with nothing on screen to explain it. That is what the CLIP
 * migration shipped as until now: it held the guard, rejected the monitor
 * commands, paused capture, and had no overlay, because this component only
 * ever polled MiniLM.
 */
export default function VectorMigrationOverlay() {
  const { t } = useTranslation();
  // `{ kind, icon, status }`, or null when the app is not in maintenance mode.
  const [active, setActive] = useState(null);
  const [reauthenticating, setReauthenticating] = useState(false);
  const samplesRef = useRef([]);

  const poll = useCallback(async () => {
    try {
      const maintenance = await getMaintenanceStatus();
      if (!maintenance?.active) {
        setActive(null);
        samplesRef.current = [];
        return;
      }
      const kind = MIGRATION_KINDS[maintenance.reason];
      // A detailed read that fails costs the box its progress bar, not its
      // presence: the app is demonstrably in maintenance mode either way.
      let next = null;
      if (kind) {
        try {
          next = await kind.read();
        } catch {
          next = null;
        }
      }
      setActive({ kind: kind?.id ?? 'unknown', icon: kind?.icon, status: next });

      const source = PROGRESS_SOURCES[next?.phase];
      if (next?.running && source) {
        const value = next[source[0]] ?? 0;
        const samples = samplesRef.current;
        const last = samples[samples.length - 1];
        if (!last || last.value !== value) {
          samples.push({ at: Date.now(), value, phase: next.phase });
          if (samples.length > 30) samples.shift();
        }
      } else {
        samplesRef.current = [];
      }
    } catch {
      // Status polling must never break the overlay; keep the last snapshot.
    }
  }, []);

  const running = Boolean(active?.status?.running);
  useEffect(() => {
    let timer;
    let cancelled = false;
    const tick = async () => {
      await poll();
      if (cancelled) return;
      timer = setTimeout(tick, running ? ACTIVE_POLL_MS : IDLE_POLL_MS);
    };
    tick();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [poll, running]);

  if (!active) return null;

  const status = active.status ?? {};
  const kindKey = `vectorMigration.kinds.${active.kind}`;
  const phase = status.phase || 'starting';
  const source = PROGRESS_SOURCES[phase];
  const current = source ? status[source[0]] ?? 0 : 0;
  const total = source ? status[source[1]] ?? 0 : 0;
  const percent = total > 0 ? Math.min(100, Math.round((current / total) * 100)) : null;

  // ETA from the observed processing rate within the current phase only.
  let eta = null;
  const samples = samplesRef.current.filter((sample) => sample.phase === phase);
  if (total > 0 && samples.length >= 2) {
    const first = samples[0];
    const last = samples[samples.length - 1];
    const rate = (last.value - first.value) / Math.max(1, (last.at - first.at) / 1000);
    if (rate > 0) eta = formatEta((total - current) / rate);
  }

  const waitingForAuth = phase === 'waiting_for_auth';
  const isPublishSync = phase === 'publishing_sync';
  const errorCount =
    (status.failed ?? 0) + (status.unmappable ?? 0) + (status.discarded ?? 0);
  const phaseText = t(`vectorMigration.phases.${phase}`, {
    defaultValue: t('vectorMigration.phases.working'),
  });

  // This overlay covers AuthMask (z-50), so while the run is stuck in
  // waiting_for_auth it must offer its own way to bring up Windows Hello;
  // the backend worker polls the session and resumes on its own.
  const handleReauthenticate = async () => {
    setReauthenticating(true);
    try {
      await requestAuth();
    } catch {
      // Cancelled or failed: the button simply becomes usable again.
    } finally {
      setReauthenticating(false);
    }
  };

  const KindIcon = active.icon ?? Wrench;
  const HeaderIcon = waitingForAuth ? ShieldAlert : KindIcon;

  return (
    <div className="absolute inset-0 z-[200] flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-lg bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl flex flex-col">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1 shrink-0">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center shrink-0">
            <HeaderIcon
              className={cn('w-5 h-5', waitingForAuth ? 'text-amber-400' : 'text-ide-accent')}
            />
          </div>
          <div className="min-w-0">
            <h2 className="text-lg font-semibold text-ide-text">
              {t(`${kindKey}.title`)}
            </h2>
            <p className="text-xs text-ide-muted">
              {t(`${kindKey}.subtitle`)}
            </p>
          </div>
        </div>

        {/* Content */}
        <div className="mt-4 space-y-3">
          <div className="w-full space-y-1.5 pt-2">
            <div className="flex justify-between text-xs">
              <span className="text-ide-text font-medium">{phaseText}</span>
              {percent !== null && (
                <span className="text-ide-muted tabular-nums">
                  {current.toLocaleString()} / {total.toLocaleString()} ({percent}%)
                </span>
              )}
            </div>
            <div className="w-full h-2 bg-ide-bg rounded-full overflow-hidden border border-ide-border">
              <div
                className={cn(
                  'h-full bg-ide-accent rounded-full transition-all duration-300 ease-out',
                  percent === null && 'w-1/3 animate-pulse'
                )}
                style={percent !== null ? { width: `${percent}%` } : undefined}
              />
            </div>
            {eta && (
              <p className="text-xs text-ide-muted">
                {t('vectorMigration.eta', { eta })}
              </p>
            )}
          </div>

          {isPublishSync && (
            <div className="p-3 bg-amber-500/10 border border-amber-500/20 rounded-lg">
              <p className="text-xs text-amber-400 leading-relaxed">
                {t('vectorMigration.safeWrite')}
              </p>
            </div>
          )}

          {waitingForAuth && (
            <div className="p-3 bg-amber-500/10 border border-amber-500/20 rounded-lg">
              <p className="text-xs text-amber-400 leading-relaxed">
                {t('vectorMigration.waitingForAuth')}
              </p>
            </div>
          )}

          {(errorCount > 0 || status.last_error) && (
            <div className="text-xs px-3 py-2 rounded bg-red-500/10 text-red-400 min-w-0">
              {errorCount > 0 && (
                <p>{t('vectorMigration.errorCount', { count: errorCount })}</p>
              )}
              {status.last_error && (
                <p className="truncate" title={status.last_error}>{status.last_error}</p>
              )}
            </div>
          )}

          <p className="text-[11px] text-ide-muted">
            {t('vectorMigration.blockedHint')}
          </p>
        </div>

        {/* Actions */}
        {waitingForAuth && (
          <div className="mt-5 flex items-center justify-end gap-2 shrink-0">
            <button
              onClick={handleReauthenticate}
              disabled={reauthenticating}
              className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
            >
              {reauthenticating ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <KeyRound className="w-3.5 h-3.5" />
              )}
              {t('vectorMigration.reauth')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
