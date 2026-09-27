import { useCallback, useEffect, useState } from 'react';
import {
  aiErrorDetail, aiErrorKey, deleteAiProvider, getAiSettings, saveAiProvider, setDefaultAiProvider, testAiProvider,
} from '../../../lib/ai_api';

export const PROVIDER_PRESETS = [
  { id: 'anthropic', name: 'Claude', kind: 'anthropic', baseUrl: 'https://api.anthropic.com', model: 'claude-opus-5' },
  { id: 'deepseek', name: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1', model: 'deepseek-chat' },
  { id: 'qwen', name: 'Qwen', baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1', model: 'qwen-plus' },
  { id: 'kimi', name: 'Kimi', baseUrl: 'https://api.moonshot.cn/v1', model: 'kimi-k2-0905-preview' },
  { id: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1', model: '' },
  { id: 'openrouter', name: 'OpenRouter', baseUrl: 'https://openrouter.ai/api/v1', model: '' },
  { id: 'ollama', name: 'Ollama', baseUrl: 'http://localhost:11434/v1', model: '' },
  { id: 'lmstudio', name: 'LM Studio', baseUrl: 'http://localhost:1234/v1', model: '' },
];

const emptyDraft = () => ({ id: null, name: '', kind: 'openai_compatible', baseUrl: '', model: '', apiKey: '', hasApiKey: false, keyTouched: false });

function toInput(draft) {
  return { ...draft, apiKey: draft.keyTouched ? draft.apiKey : undefined };
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
    setDraft({ ...emptyDraft(), id: provider.id, name: provider.name, kind: provider.kind, baseUrl: provider.base_url, model: provider.model, hasApiKey: provider.has_api_key });
    setMessage(null);
  };
  const cancelEdit = () => { setDraft(null); setMessage(null); };
  const updateDraft = (patch) => setDraft((current) => ({ ...current, ...patch, ...('apiKey' in patch ? { keyTouched: true } : {}) }));
  const applyPreset = (presetId) => {
    const preset = PROVIDER_PRESETS.find((item) => item.id === presetId);
    if (preset) {
      setDraft((current) => ({
        ...current, name: preset.name, kind: preset.kind || 'openai_compatible', baseUrl: preset.baseUrl, model: preset.model || current.model,
      }));
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
