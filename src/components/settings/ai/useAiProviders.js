import { useCallback, useEffect, useState } from 'react';
import {
  aiErrorDetail, aiErrorKey, deleteAiProvider, getAiSettings, saveAiProvider, setDefaultAiProvider, testAiProvider,
  DEFAULT_AI_CONTEXT_TOKENS,
} from '../../../lib/ai_api';
import { CLOUD_AI_CONTEXT_TOKENS, PROVIDER_PRESETS, findProviderPreset } from '../../../lib/ai_provider_catalog';

const emptyDraft = () => ({ id: null, presetId: '', customModel: false, name: '', kind: 'openai_compatible', baseUrl: '', model: '', apiKey: '', hasApiKey: false, keyTouched: false, contextTokens: DEFAULT_AI_CONTEXT_TOKENS });

function toInput(draft) {
  return { ...draft, apiKey: draft.keyTouched ? draft.apiKey : undefined, contextTokens: Number(draft.contextTokens) };
}

/** State for the model service list and its inline editor. */
export function useAiProviders({ t }) {
  const [settings, setSettings] = useState(null);
  const [loadError, setLoadError] = useState('');
  const [draft, setDraft] = useState(null);
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState(null);

  const describe = useCallback((error) => {
    const detail = aiErrorDetail(error);
    return t(aiErrorKey(error)) + (detail ? ` (${detail})` : '');
  }, [t]);

  const load = useCallback(async () => {
    try {
      setSettings(await getAiSettings());
      setLoadError('');
    } catch (error) {
      setLoadError(describe(error));
    }
  }, [describe]);

  useEffect(() => { load(); }, [load]);

  const run = async (name, action) => {
    setBusy(name);
    setMessage(null);
    try {
      return await action();
    } catch (error) {
      setMessage({ tone: 'error', text: describe(error) });
      return undefined;
    } finally {
      setBusy('');
    }
  };

  const startAdd = () => { setDraft(emptyDraft()); setMessage(null); };
  const startEdit = (provider) => {
    const preset = findProviderPreset(provider.kind, provider.base_url);
    setDraft({ ...emptyDraft(), presetId: preset?.id || 'custom', customModel: !preset?.models.some((model) => model.id === provider.model), id: provider.id, name: provider.name, kind: provider.kind, baseUrl: provider.base_url, model: provider.model, hasApiKey: provider.has_api_key, contextTokens: provider.context_tokens ?? DEFAULT_AI_CONTEXT_TOKENS });
    setMessage(null);
  };
  const cancelEdit = () => { setDraft(null); setMessage(null); };
  const updateDraft = (patch) => setDraft((current) => ({ ...current, ...patch, ...('apiKey' in patch ? { keyTouched: true } : {}) }));
  const applyPreset = (presetId) => {
    setMessage(null);
    if (presetId === 'custom') {
      setDraft({ ...emptyDraft(), presetId });
      return;
    }
    const preset = PROVIDER_PRESETS.find((item) => item.id === presetId);
    if (preset) {
      setDraft({
        ...emptyDraft(), presetId, name: preset.name, kind: preset.kind || 'openai_compatible', baseUrl: preset.baseUrl,
        model: preset.models.find((model) => model.recommended)?.id || '',
        contextTokens: preset.local ? DEFAULT_AI_CONTEXT_TOKENS : CLOUD_AI_CONTEXT_TOKENS,
      });
    }
  };

  const save = () => run('save', async () => {
    const next = await saveAiProvider(toInput(draft));
    setSettings(next);
    setDraft(null);
    setMessage({ tone: 'success', text: t('settings.ai.saved') });
  });

  const test = () => run('test', async () => {
    const result = await testAiProvider(toInput(draft));
    if (!result.ok) {
      setMessage({ tone: 'error', text: describe(result.error) });
      return;
    }
    const key = result.tool_calling === 'supported' ? 'settings.ai.test.ok' : 'settings.ai.test.ok_without_tools';
    setMessage({ tone: result.tool_calling === 'supported' ? 'success' : 'warning', text: t(key, { ms: result.latency_ms }) });
    if (draft.id) load();
  });

  const remove = (id) => run('delete:' + id, async () => {
    setSettings(await deleteAiProvider(id));
    if (draft?.id === id) setDraft(null);
  });

  const makeDefault = (id) => run('default:' + id, async () => {
    setSettings(await setDefaultAiProvider(id));
  });

  return {
    settings, loadError, draft, busy, message,
    load, startAdd, startEdit, cancelEdit, updateDraft, applyPreset, save, test, remove, makeDefault,
  };
}
