import React, { useCallback, useEffect, useRef, useState } from 'react';
import { CalendarDays } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { getAiSettings } from '../../lib/ai_api';
import { getRecapDay, getRecapSettings, localDate, recapErrorKey, saveRecapSettings } from '../../lib/recap_api';
import { openSettingsWindow } from '../../lib/settings_api';
import { useSettingsActive, useSettingsActivity, useSettingsWindowActivity } from './SettingsActivityContext';
import { SettingsButton, SettingsSelect, SettingsSwitch } from './SettingsControls';
import { SettingsDisclosure, SettingsDivider, SettingsErrorBanner, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus } from './SettingsPrimitives';

const limits = {
  request_output_tokens: [1000, 64000], answer_tokens: [512, 16000], batch_input_tokens: [4000, 1000000],
  batch_output_tokens: [1000, 64000], daily_input_tokens: [1000, 10000000], daily_output_tokens: [1000, 1000000], screening_input_tokens: [1000, 20000000],
};
const inputClass = 'w-full rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-sm disabled:opacity-50';

const sameSettings = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const invalidBudget = (settings) => Object.entries(limits).some(([key, [min, max]]) => !Number.isInteger(Number(settings[key])) || Number(settings[key]) < min || Number(settings[key]) > max)
  || settings.daily_input_tokens < settings.batch_input_tokens || settings.daily_output_tokens < settings.batch_output_tokens;
const invalidScreening = ({ screening }) => {
  if (!screening.enabled) return false;
  try {
    const url = new URL(screening.base_url);
    return !['http:', 'https:'].includes(url.protocol) || !screening.model.trim() || screening.model.length > 200 || screening.base_url.length > 512 || (screening.api_key?.length || 0) > 2048;
  } catch { return true; }
};

// Apply server normalization (including clearing saved keys) without replacing newer edits.
function reconcileSettings(current, submitted, saved) {
  const merge = (current, submitted, saved) => {
    const next = { ...saved };
    for (const key of new Set([...Object.keys(current), ...Object.keys(submitted)])) {
      if (current[key] === submitted[key]) continue;
      if (key in current) next[key] = current[key];
      else delete next[key];
    }
    return next;
  };
  return { ...merge(current, submitted, saved), screening: merge(current.screening, submitted.screening, saved.screening) };
}

export default function RecapSection() {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const { enabled } = useSettingsWindowActivity();
  const [saved, setSaved] = useState(null);
  const [draft, setDraft] = useState(null);
  const [providers, setProviders] = useState([]);
  const [usage, setUsage] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const dirty = Boolean(draft && !sameSettings(draft, saved));
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const draftRef = useRef(null);
  const savedRef = useRef(null);
  const savingRef = useRef(false);
  const pendingSave = useRef(null);
  const epoch = useRef(0);
  const readSequence = useRef(0);
  useEffect(() => () => { epoch.current += 1; }, []);
  useSettingsActivity('daily-recap', { dirty, busy });
  useEffect(() => {
    if (!enabled) {
      epoch.current += 1;
      draftRef.current = savedRef.current = pendingSave.current = null;
      savingRef.current = dirtyRef.current = false;
      setSaved(null); setDraft(null); setProviders([]); setUsage(null); setError(''); setBusy(false);
    }
  }, [enabled]);
  const load = useCallback(async () => {
    const token = epoch.current;
    const request = ++readSequence.current;
    try {
      const [settings, ai, day] = await Promise.all([getRecapSettings(), getAiSettings(), getRecapDay(localDate(), { includeRecords: false, includeAttempts: false }).catch(() => null)]);
      if (token !== epoch.current || request !== readSequence.current) return;
      if (!dirtyRef.current && !savingRef.current) { savedRef.current = draftRef.current = settings; setSaved(settings); setDraft(settings); }
      setProviders(ai.providers || []); setUsage(day?.usage || null); setError('');
    } catch (cause) { if (token === epoch.current && request === readSequence.current) setError(String(cause)); }
  }, []);
  useEffect(() => {
    if (!active) return undefined;
    load();
    return undefined;
  }, [active, load]);
  const save = async (settings = draftRef.current) => {
    if (!enabled || !settings || invalidBudget(settings) || invalidScreening(settings)) return;
    pendingSave.current = settings;
    if (savingRef.current) return;
    const token = epoch.current;
    savingRef.current = true;
    setBusy(true); setError('');
    try {
      while (pendingSave.current) {
        const submitted = pendingSave.current;
        pendingSave.current = null;
        if (sameSettings(submitted, savedRef.current)) continue;
        readSequence.current += 1;
        const next = await saveRecapSettings(submitted);
        if (token !== epoch.current) return;
        readSequence.current += 1;
        savedRef.current = next;
        draftRef.current = reconcileSettings(draftRef.current, submitted, next);
        if (pendingSave.current) pendingSave.current = reconcileSettings(pendingSave.current, submitted, next);
        dirtyRef.current = !sameSettings(draftRef.current, next);
        setSaved(next); setDraft(draftRef.current);
      }
    } catch (cause) {
      if (token === epoch.current) { pendingSave.current = null; setError(String(cause)); }
    } finally {
      if (token === epoch.current) { savingRef.current = false; setBusy(false); }
    }
  };
  const update = (key, value, commit = true) => {
    const next = { ...draftRef.current, [key]: value };
    draftRef.current = next;
    dirtyRef.current = !sameSettings(next, savedRef.current);
    setDraft(next); setError('');
    if (commit) save(next);
  };
  const screen = (key, value, commit = true) => update('screening', { ...draftRef.current.screening, [key]: value }, commit);
  const commitInput = { onBlur: () => save(), onKeyDown: (event) => { if (event.key === 'Enter') event.currentTarget.blur(); } };
  return <SettingsSection id="daily-recap" icon={CalendarDays} title={t('recap.title')} description={t('recap.settings.description')}>
    {error && <SettingsErrorBanner>{t(recapErrorKey(error))}<SettingsButton disabled={busy} onClick={() => draft ? save() : load()}>{t('common.retry')}</SettingsButton></SettingsErrorBanner>}
    {!draft ? <SettingsStatus>{t('recap.loading')}</SettingsStatus> : <SettingsGroup aria-label={t('recap.settings.title')}>
        <SettingsRow label={t('recap.settings.enabled')} description={t('recap.settings.enabledHint')}
          control={<SettingsSwitch checked={draft.enabled} onChange={(value) => update('enabled', value)} />} />
        <SettingsDivider />
        <div className="space-y-5">
          <SettingsRow label={t('recap.settings.provider')} control={<SettingsSelect value={draft.provider_id || ''}
            onChange={(value) => update('provider_id', value || null)} options={[
              { value: '', label: t('recap.settings.defaultProvider') },
              ...providers.map((provider) => ({ value: provider.id, label: provider.name })),
              ...(draft.provider_id && !providers.some((p) => p.id === draft.provider_id) ? [{ value: draft.provider_id, label: t('recap.settings.missingProvider'), disabled: true }] : []),
            ]} />}>
            <SettingsButton variant="ghost" onClick={() => openSettingsWindow('ai').catch((cause) => setError(String(cause)))}>{t('recap.settings.manageModels')}</SettingsButton>
          </SettingsRow>
        </div>
        <SettingsDivider />
        <SettingsDisclosure title={t('recap.settings.advanced')}>
          <p className="text-xs leading-relaxed text-ide-muted">{t('recap.settings.budgetHint')}</p>
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            {Object.entries(limits).map(([key, [min, max]]) => <label key={key} className="block space-y-2 text-xs"><span>{t(`recap.settings.${key}`)}</span><input className={inputClass} type="number" min={min} max={max} step={1} required value={draft[key]} {...commitInput}
              onChange={(event) => update(key, event.target.value === '' ? '' : Number(event.target.value), false)} /></label>)}
          </div>
          {invalidBudget(draft) && <SettingsStatus tone="error">{t('recap.errors.RECAP_INVALID_SETTINGS')}</SettingsStatus>}
          {usage && <SettingsStatus>{t('recap.settings.usage', { input: usage.input_tokens, output: usage.output_tokens, screening: usage.screening_input_tokens })}</SettingsStatus>}
          <SettingsDivider />
          <SettingsRow label={t('recap.settings.screening')} description={t('recap.settings.screeningHint')}
            control={<SettingsSwitch checked={draft.screening.enabled} onChange={(value) => screen('enabled', value)} />} />
          {draft.screening.enabled && <div className="space-y-3">
            <label className="block space-y-2 text-xs"><span>{t('recap.settings.endpoint')}</span><input className={inputClass} type="url" required value={draft.screening.base_url} {...commitInput} onChange={(event) => screen('base_url', event.target.value, false)} /></label>
            <label className="block space-y-2 text-xs"><span>{t('recap.settings.model')}</span><input className={inputClass} required value={draft.screening.model} {...commitInput} onChange={(event) => screen('model', event.target.value, false)} /></label>
            <label className="block space-y-2 text-xs"><span>{t('recap.settings.apiKey')}</span><input className={inputClass} type="password" autoComplete="off" value={draft.screening.api_key ?? ''} {...commitInput}
              placeholder={t(draft.screening.has_api_key ? 'recap.settings.keySaved' : 'recap.settings.keyPlaceholder')}
              onChange={(event) => { const screening = { ...draftRef.current.screening }; if (event.target.value) screening.api_key = event.target.value; else delete screening.api_key; update('screening', screening, false); }} /></label>
            {invalidScreening(draft) && <SettingsStatus tone="error">{t('recap.errors.RECAP_INVALID_SETTINGS')}</SettingsStatus>}
          </div>}
        </SettingsDisclosure>
      </SettingsGroup>}
  </SettingsSection>;
}
