import React, { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Image as ImageIcon,
  KeyRound,
  SearchCheck,
  ShieldAlert,
  Wrench,
} from 'lucide-react';
import { getBlindIndexRepairStatus, getMaintenanceStatus } from '../lib/semantic_api';
import { requestAuth } from '../lib/auth_api';
import { usePolling } from '../hooks/usePolling';
import { OverlayShell, ProgressBlock } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

const ACTIVE_POLL_MS = 1000;
const IDLE_POLL_MS = 3000;
// Phases whose progress pair drives the bar and the ETA estimate.
const PROGRESS_SOURCES = {
  repairing_blind_index: ['processed', 'total'],
};

/**
 * Which detailed status to read, keyed by the reason string the backend passes
 * to `maintenance::enter`. Keeping the two in step is what stops this overlay
 * from going blank the next time a maintenance task is added: an unrecognised
 * reason still renders a box, just without progress.
 */
const MIGRATION_KINDS = {
  clip_ann_bootstrap: { id: 'clip', icon: ImageIcon, read: null, phase: 'building_ann' },
  blind_index_repair: {
    id: 'blindIndex',
    icon: SearchCheck,
    read: getBlindIndexRepairStatus,
  },
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
 * Full-window, non-dismissable maintenance overlay for startup index
 * maintenance. Runs cannot be cancelled: closing the app merely interrupts
 * them, and they resume on the next launch/unlock.
 *
 * Visibility is decided by *maintenance mode*, not by any one task's `running`
 * flag. The guard is taken before a run marks itself running and dropped after
 * it clears that flag, so maintenance strictly contains every run — and gating
 * on the outer condition means the app can never sit in maintenance mode with
 * nothing on screen to explain it.
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
      let next = kind?.phase ? { running: true, phase: kind.phase } : null;
      if (kind?.read) {
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
  usePolling(poll, { intervalMs: running ? ACTIVE_POLL_MS : IDLE_POLL_MS });

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
  const errorCount = status.failed ?? 0;
  const phaseText = t(`vectorMigration.phases.${phase}`, {
    defaultValue: t('vectorMigration.phases.working'),
  });

  // This overlay covers AuthMask (the gate layer), so while the run is stuck in
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

  return (
    <OverlayShell
      layer="maintenance"
      icon={waitingForAuth ? ShieldAlert : KindIcon}
      tone={waitingForAuth ? 'warning' : 'accent'}
      title={t(`${kindKey}.title`)}
      subtitle={t(`${kindKey}.subtitle`)}
      ariaLabel={t(`${kindKey}.title`)}
      footer={waitingForAuth && (
        <Button size="md" variant="primary" icon={KeyRound} loading={reauthenticating} onClick={handleReauthenticate}>
          {t('vectorMigration.reauth')}
        </Button>
      )}
    >
      <ProgressBlock
        className="pt-2"
        label={phaseText}
        current={source ? current : undefined}
        total={source ? total : undefined}
        detail={eta && t('vectorMigration.eta', { eta })}
      />

      {waitingForAuth && <Banner tone="warning" icon={false}>{t('vectorMigration.waitingForAuth')}</Banner>}

      {(errorCount > 0 || status.last_error) && (
        <Banner tone="error" icon={false}>
          {errorCount > 0 && <p>{t('vectorMigration.errorCount', { count: errorCount })}</p>}
          {status.last_error && <p className="truncate" title={status.last_error}>{status.last_error}</p>}
        </Banner>
      )}

      <p className="text-[11px] text-ide-muted">{t('vectorMigration.blockedHint')}</p>
    </OverlayShell>
  );
}
