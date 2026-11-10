import React from 'react';
import { useTranslation } from 'react-i18next';
import { KeyRound, RefreshCw } from 'lucide-react';
import { Button } from '../../ui/Button';
import { Banner } from '../../ui/Banner';
import { ProgressBlock } from '../../overlay';
import { ConfirmDialog } from '../../ConfirmDialog';
import { SettingsRow, SettingsStatus } from '../SettingsPrimitives';
import { SettingsSelect } from '../SettingsControls';
import { useUnattendedCredentials } from './useUnattendedCredentials';
import { canIssueUnattendedCredential } from './agentAccessConstants';

const PHASES = {
  ready: 'ready', preparing: 'preparing', syncing: 'preparing', reconciling: 'preparing',
  waiting_for_unlock: 'waiting', gap: 'waiting', error: 'error', unavailable: 'unavailable', disabled: 'disabled',
};

export function UnattendedCredentialsView({ credentials: c, now = Date.now() }) {
  const { t, i18n } = useTranslation();
  const status = c.status;
  const ready = status?.enabled && status.available && status.phase === 'ready';
  const canIssue = canIssueUnattendedCredential(status);
  const phase = PHASES[status?.phase] || 'preparing';
  const preparing = status?.enabled && phase === 'preparing';
  const progress = status?.preparation;
  const percent = progress?.total > 0 ? progress.processed / progress.total * 100 : undefined;
  const expiryOptions = ['1d', '30d', '90d', 'never'].map((value) => ({ value, label: t(`settings.security.credentials.expiryOptions.${value}`) }));
  const date = (value) => value == null ? t('settings.security.credentials.neverUsed') : new Date(value).toLocaleString(i18n.language);
  return <div className="rounded-xl border border-ide-border bg-ide-bg p-4 text-sm space-y-4">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div className="min-w-0 flex-1">
        <h3 className="flex items-center gap-2 font-semibold text-ide-text"><KeyRound className="h-4 w-4" aria-hidden="true" />{t('settings.security.credentials.title')}</h3>
        <p className="mt-1 text-xs leading-relaxed text-ide-muted">{t('settings.security.credentials.description')}</p>
      </div>
      <Button variant="ghost" onClick={c.openSecurity}>{t('settings.security.credentials.manageLevel')}</Button>
    </div>
    {c.loading && <ProgressBlock label={t('settings.security.feedback.loading')} showCount={false} />}
    {status && <SettingsStatus tone={status.enabled ? (ready ? 'success' : 'warning') : 'neutral'}>{t(`settings.security.access.phases.${phase}`)}</SettingsStatus>}
    {status?.enabled && <Banner tone="warning">{t('settings.security.access.active')}</Banner>}
    {status && !status.available && <Banner tone="info">{t('settings.security.access.requirements')}</Banner>}
    {preparing && <ProgressBlock label={t('settings.security.access.preparing')} percent={percent} showCount={false} />}
    {status?.enabled && !ready && !preparing && <Button icon={RefreshCw} loading={c.busy} disabled={!status.available} onClick={c.resume}>{t('settings.security.access.resume')}</Button>}
    {canIssue && <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); c.issue(); }}>
      {!ready && <Banner tone="info">{t('settings.security.credentials.preparing')}</Banner>}
      {c.replacement && <Banner tone="warning">{t('settings.security.credentials.replaceMessage', { label: c.replacement.label })}</Banner>}
      <SettingsRow label={t('settings.security.credentials.label')} control={<input value={c.label} maxLength={80} aria-label={t('settings.security.credentials.label')}
        disabled={c.busy} onChange={(event) => c.setLabel(event.target.value)}
        className="min-h-9 w-52 max-w-full rounded-lg border border-ide-border bg-ide-panel px-3 py-1.5 text-sm text-ide-text focus:outline-none focus:border-ide-accent" />} />
      <SettingsRow label={t('settings.security.credentials.expiry')} control={<SettingsSelect value={c.expiry} options={expiryOptions} disabled={c.busy} onChange={c.setExpiry} />} />
      <div className="flex flex-wrap justify-end gap-2">
        {c.replacement && <Button disabled={c.busy} onClick={c.cancelReplacement}>{t('common.cancel')}</Button>}
        <Button type="submit" variant="primary" loading={c.busy} disabled={!c.label.trim()}>{t(c.replacement ? 'settings.security.credentials.replaceAndCopy' : 'settings.security.credentials.createAndCopy')}</Button>
      </div>
      <p className="text-xs leading-relaxed text-ide-muted">{t('settings.security.credentials.delivery')}</p>
    </form>}
    {status?.enabled && status.grants?.length === 0 && <SettingsStatus>{t('settings.security.credentials.empty')}</SettingsStatus>}
    {status?.enabled && status.grants?.length > 0 && <ul className="space-y-2">
      {status.grants.map((grant) => {
        const expired = grant.expires_at_ms != null && grant.expires_at_ms <= now;
        const grantState = grant.revoked ? 'revoked' : expired ? 'expired' : 'active';
        return <li key={grant.id} className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-ide-border p-3">
          <div className="min-w-0 flex-1 text-xs text-ide-muted">
            <p className="break-words font-medium text-ide-text">{grant.label} <span className={grantState === 'active' ? 'text-ide-info-success' : 'text-ide-muted'}>{t(`settings.security.credentials.states.${grantState}`)}</span></p>
            <p className="mt-1">{t('settings.security.credentials.expires', { date: grant.expires_at_ms == null ? t('settings.security.credentials.expiryOptions.never') : date(grant.expires_at_ms) })}</p>
            <p>{t('settings.security.credentials.lastUsed', { date: date(grant.last_used_at_ms) })}</p>
          </div>
          {!grant.revoked && <div className="flex flex-wrap gap-2">
            <Button disabled={c.busy || !canIssue} onClick={() => c.replace(grant)}>{t('settings.security.credentials.replace')}</Button>
            <Button variant="danger" disabled={c.busy} onClick={() => c.requestRevoke(grant)}>{t('settings.security.credentials.revoke')}</Button>
          </div>}
        </li>;
      })}
    </ul>}
    {c.notice && <Banner tone="success">{c.notice}</Banner>}
    {c.error && <Banner tone="error">{c.error}</Banner>}
    {!c.loading && !status && <Button onClick={c.refresh}>{t('common.retry')}</Button>}
    <ConfirmDialog isOpen={Boolean(c.revocation)} loading={c.busy} onConfirm={c.revoke} onCancel={c.cancelRevoke}
      title={t('settings.security.credentials.revokeTitle')} message={t('settings.security.credentials.revokeMessage', { label: c.revocation?.label })}
      confirmVariant="danger" confirmLabel={t('settings.security.credentials.revoke')} cancelLabel={t('common.cancel')}
      loadingLabel={t('settings.security.feedback.applying')} />
  </div>;
}

export default function UnattendedCredentialsCard() {
  const credentials = useUnattendedCredentials();
  return <UnattendedCredentialsView credentials={credentials} />;
}
