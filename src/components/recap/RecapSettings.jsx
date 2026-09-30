import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';

const inputClass = 'w-full rounded-lg border border-ide-border bg-ide-bg px-3 py-2 text-sm';

export default function RecapSettings({ settings, providers, busy, onSave, onClose, usage }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(() => ({ ...settings, screening: { ...settings.screening } }));
  const update = (key, value) => setDraft((d) => ({ ...d, [key]: value }));
  const screen = (key, value) => setDraft((d) => ({ ...d, screening: { ...d.screening, [key]: value } }));
  const fields = ['request_output_tokens', 'answer_tokens', 'batch_input_tokens', 'batch_output_tokens', 'daily_input_tokens', 'daily_output_tokens', 'screening_input_tokens'];
  return (
    <form aria-label={t('recap.settings.title')} onSubmit={(e) => { e.preventDefault(); onSave(draft); }} className="space-y-5 rounded-xl border border-ide-border bg-ide-panel p-5">
      <div>
        <h2 className="font-medium">{t('recap.settings.title')}</h2>
        <p className="mt-1 text-sm leading-relaxed text-ide-muted">{t('recap.settings.description')}</p>
      </div>
      <label className="flex items-center gap-3 text-sm"><input type="checkbox" checked={draft.enabled} disabled={busy} onChange={(e) => update('enabled', e.target.checked)} />{t('recap.settings.enabled')}</label>
      <label className="block space-y-1 text-sm"><span>{t('recap.settings.provider')}</span>
        <select value={draft.provider_id || ''} disabled={busy} onChange={(e) => update('provider_id', e.target.value || null)} className={inputClass}>
          <option value="">{t('recap.settings.defaultProvider')}</option>
          {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </label>
      <label className="block space-y-1 text-sm"><span>{t('recap.settings.language')}</span>
        <select className={inputClass} value={draft.language} onChange={(e) => update('language', e.target.value)}><option value="zh-CN">中文</option><option value="en">English</option></select>
      </label>
      <details className="space-y-4">
        <summary className="cursor-pointer text-sm text-ide-muted">{t('recap.settings.advanced')}</summary>
        <p className="text-xs leading-relaxed text-ide-muted">{t('recap.settings.budgetHint')}</p>
        <p className="text-xs text-ide-muted">{t('recap.settings.dailyTotal', { total: draft.daily_input_tokens + draft.daily_output_tokens })}</p>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {fields.map((key) => <label key={key} className="block space-y-1 text-xs"><span>{t(`recap.settings.${key}`)}</span><input className={inputClass} type="number" min={key === 'answer_tokens' ? 512 : key === 'batch_input_tokens' ? 4000 : 1000} step={key === 'answer_tokens' ? 1 : 1000} required value={draft[key]} disabled={busy} onChange={(e) => update(key, Number(e.target.value))} /></label>)}
        </div>
        {usage && <p className="text-xs text-ide-muted">{t('recap.settings.usage', { input: usage.input_tokens, output: usage.output_tokens, screening: usage.screening_input_tokens })}</p>}
        <label className="flex items-center gap-3 text-sm"><input type="checkbox" checked={draft.screening.enabled} onChange={(e) => screen('enabled', e.target.checked)} />{t('recap.settings.screening')}</label>
        <p className="text-xs text-ide-muted">{t('recap.settings.screeningHint')}</p>
        {draft.screening.enabled && <div className="space-y-3">
          <label className="block space-y-1 text-xs"><span>{t('recap.settings.endpoint')}</span><input className={inputClass} type="url" required value={draft.screening.base_url} onChange={(e) => screen('base_url', e.target.value)} /></label>
          <label className="block space-y-1 text-xs"><span>{t('recap.settings.model')}</span><input className={inputClass} required value={draft.screening.model} onChange={(e) => screen('model', e.target.value)} /></label>
          <label className="block space-y-1 text-xs"><span>{t('recap.settings.apiKey')}</span><input className={inputClass} type="password" autoComplete="off" value={draft.screening.api_key ?? ''} placeholder={t(draft.screening.has_api_key ? 'recap.settings.keySaved' : 'recap.settings.keyPlaceholder')} onChange={(e) => screen('api_key', e.target.value)} /></label>
        </div>}
      </details>
      <div className="flex justify-end gap-2"><button type="button" onClick={onClose} className="rounded-lg px-4 py-2 text-sm hover:bg-ide-hover">{t('recap.close')}</button><button disabled={busy} className="rounded-lg bg-ide-accent px-4 py-2 text-sm text-white disabled:opacity-50">{t('recap.save')}</button></div>
    </form>
  );
}
