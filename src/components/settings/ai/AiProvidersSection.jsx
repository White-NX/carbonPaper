import React, { useId } from 'react';
import { useTranslation } from 'react-i18next';
import { Bot, Check, Pencil, Plus, Trash2, PlugZap } from 'lucide-react';
import { SettingsButton, SettingsSelect } from '../SettingsControls';
import { SettingsErrorBanner, SettingsGroup, SettingsSection, SettingsStatus } from '../SettingsPrimitives';
import { PROVIDER_PRESETS, useAiProviders } from './useAiProviders';
import { MIN_AI_CONTEXT_TOKENS, MAX_AI_CONTEXT_TOKENS } from '../../../lib/ai_api';

const inputClass = 'w-full min-w-0 rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-sm placeholder:text-ide-muted disabled:opacity-50';

function Field({ label, children, hint }) {
  const id = useId();
  return (
    <div className="space-y-1">
      <label htmlFor={id} className="block text-xs font-medium text-ide-text">{label}</label>
      {React.cloneElement(children, { id })}
      {hint && <p className="text-xs leading-relaxed text-ide-muted">{hint}</p>}
    </div>
  );
}

function hostOf(url) {
  try { return new URL(url).host; } catch { return url; }
}

function ProviderEditor({ c, t }) {
  const { draft, busy } = c;
  const disabled = Boolean(busy);
  const incomplete = !draft.baseUrl.trim() || !draft.model.trim();
  const contextTokens = Number(draft.contextTokens);
  const invalidBudget = !Number.isInteger(contextTokens) || contextTokens < MIN_AI_CONTEXT_TOKENS || contextTokens > MAX_AI_CONTEXT_TOKENS;
  return (
    <SettingsGroup className="space-y-3">
      {!draft.id && (
        <Field label={t('settings.ai.fields.preset')}>
          <SettingsSelect value="" disabled={disabled} label={t('settings.ai.fields.preset')} onChange={c.applyPreset}
            options={[{ value: '', label: t('settings.ai.fields.preset_placeholder') }, ...PROVIDER_PRESETS.map((p) => ({ value: p.id, label: p.name }))]} />
        </Field>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t('settings.ai.fields.name')}>
          <input value={draft.name} disabled={disabled} placeholder={t('settings.ai.fields.name_placeholder')}
            onChange={(e) => c.updateDraft({ name: e.target.value })} className={inputClass} />
        </Field>
        <Field label={t('settings.ai.fields.model')}>
          <input value={draft.model} disabled={disabled} placeholder={draft.kind === 'anthropic' ? 'claude-opus-5' : 'deepseek-chat'}
            onChange={(e) => c.updateDraft({ model: e.target.value })} className={inputClass} spellCheck={false} />
        </Field>
      </div>
      <Field label={t('settings.ai.fields.kind')}>
        <SettingsSelect value={draft.kind} disabled={disabled} label={t('settings.ai.fields.kind')} onChange={(kind) => c.updateDraft({ kind })}
          options={[
            { value: 'openai_compatible', label: t('settings.ai.kinds.openai_compatible') },
            { value: 'anthropic', label: t('settings.ai.kinds.anthropic') },
          ]} />
      </Field>
      <Field label={t('settings.ai.fields.base_url')} hint={t(draft.kind === 'anthropic' ? 'settings.ai.fields.base_url_hint_anthropic' : 'settings.ai.fields.base_url_hint')}>
        <input value={draft.baseUrl} disabled={disabled} placeholder={draft.kind === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.example.com/v1'}
          onChange={(e) => c.updateDraft({ baseUrl: e.target.value })} className={inputClass} spellCheck={false} />
      </Field>
      <Field label={t('settings.ai.fields.api_key')}>
        <input type="password" autoComplete="off" value={draft.apiKey} disabled={disabled}
          placeholder={draft.hasApiKey ? t('settings.ai.fields.api_key_saved') : t('settings.ai.fields.api_key_placeholder')}
          onChange={(e) => c.updateDraft({ apiKey: e.target.value })} className={inputClass} />
      </Field>
      <details className="space-y-3">
        <summary className="cursor-pointer text-xs font-medium text-ide-muted">{t('settings.ai.advanced')}</summary>
        <Field label={t('settings.ai.fields.context_tokens')} hint={t('settings.ai.fields.context_tokens_hint')}>
          <input type="number" min={MIN_AI_CONTEXT_TOKENS} max={MAX_AI_CONTEXT_TOKENS} step="1"
            value={draft.contextTokens} disabled={disabled} aria-invalid={invalidBudget}
            onChange={(e) => c.updateDraft({ contextTokens: e.target.value })} className={inputClass} />
        </Field>
        {invalidBudget && <p role="alert" className="text-xs text-ide-error">{t('ai.errors.AI_INVALID_CONTEXT_BUDGET')}</p>}
      </details>
      <div className="flex flex-wrap justify-end gap-2">
        <SettingsButton variant="ghost" disabled={disabled} onClick={c.cancelEdit}>{t('common.cancel')}</SettingsButton>
        <SettingsButton icon={PlugZap} loading={busy === 'test'} disabled={disabled || incomplete || invalidBudget} onClick={c.test}>{t('settings.ai.test.button')}</SettingsButton>
        <SettingsButton variant="primary" loading={busy === 'save'} disabled={disabled || incomplete || invalidBudget} onClick={c.save}>{t('common.save')}</SettingsButton>
      </div>
    </SettingsGroup>
  );
}

export default function AiProvidersSection() {
  const { t } = useTranslation();
  const c = useAiProviders({ t });
  const providers = c.settings?.providers ?? [];

  return (
    <SettingsSection id="ai-providers" icon={Bot} title={t('settings.ai.providers.title')} description={t('settings.ai.providers.description')}
      actions={!c.draft && <SettingsButton icon={Plus} onClick={c.startAdd} disabled={!c.settings}>{t('settings.ai.providers.add')}</SettingsButton>}>
      {c.loadError && <SettingsErrorBanner>{c.loadError}</SettingsErrorBanner>}
      {c.settings && providers.length === 0 && !c.draft && (
        <SettingsGroup><p className="text-xs leading-relaxed text-ide-muted">{t('settings.ai.providers.empty')}</p></SettingsGroup>
      )}
      {providers.length > 0 && (
        <SettingsGroup padding="p-0" className="divide-y divide-ide-border/60">
          {providers.map((provider) => {
            const isDefault = provider.id === c.settings.default_provider_id;
            return (
              <div key={provider.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
                <div className="min-w-0 flex-1 basis-48">
                  <div className="flex flex-wrap items-center gap-2 font-medium">
                    <span className="truncate">{provider.name}</span>
                    {isDefault && <span className="rounded bg-ide-accent/15 px-1.5 py-0.5 text-[10px] text-ide-accent">{t('settings.ai.providers.default_badge')}</span>}
                    {provider.is_local && <span className="rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] text-emerald-500">{t('settings.ai.providers.local_badge')}</span>}
                  </div>
                  <p className="mt-0.5 truncate text-xs text-ide-muted">{provider.model} · {hostOf(provider.base_url)}</p>
                  {provider.tool_calling === 'unsupported' && <p className="mt-0.5 text-xs text-ide-warning">{t('settings.ai.providers.limited')}</p>}
                </div>
                <div className="flex shrink-0 gap-1">
                  {!isDefault && <SettingsButton size="xs" variant="ghost" icon={Check} loading={c.busy === 'default:' + provider.id} disabled={Boolean(c.busy)}
                    onClick={() => c.makeDefault(provider.id)}>{t('settings.ai.providers.make_default')}</SettingsButton>}
                  <SettingsButton size="xs" variant="ghost" icon={Pencil} disabled={Boolean(c.busy)} onClick={() => c.startEdit(provider)}
                    aria-label={t('settings.ai.providers.edit') + ' ' + provider.name}>{t('settings.ai.providers.edit')}</SettingsButton>
                  <SettingsButton size="xs" variant="danger" icon={Trash2} loading={c.busy === 'delete:' + provider.id} disabled={Boolean(c.busy)}
                    onClick={() => c.remove(provider.id)} aria-label={t('settings.ai.providers.delete') + ' ' + provider.name} />
                </div>
              </div>
            );
          })}
        </SettingsGroup>
      )}
      {c.draft && <ProviderEditor c={c} t={t} />}
      {c.message && <SettingsStatus tone={c.message.tone}>{c.message.text}</SettingsStatus>}
    </SettingsSection>
  );
}
