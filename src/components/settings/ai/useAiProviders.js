import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  aiErrorDetail, aiErrorKey, deleteAiProvider, discoverAiModels, getAiModelCatalog, getAiSettings,
  saveAiProvider, setDefaultAiProvider, testAiImageInput, testAiProvider, DEFAULT_AI_CONTEXT_TOKENS,
} from '../../../lib/ai_api';
import { PROVIDER_PRESETS, defaultContextTokens, findProviderPreset } from '../../../lib/ai_provider_catalog';

const emptyDraft = () => ({ id: null, presetId: '', customModel: false, name: '', kind: 'openai_compatible',
  baseUrl: '', model: '', apiKey: '', hasApiKey: false, keyTouched: false, contextTokens: DEFAULT_AI_CONTEXT_TOKENS,
  contextTokensAuto: true,
  allowImages: false, imageInputOverride: 'auto', imageInput: 'unknown' });

function toInput(draft) {
  return { ...draft, apiKey: draft.keyTouched ? draft.apiKey : undefined,
    contextTokens: draft.contextTokensAuto ? null : Number(draft.contextTokens) };
}

/** Provider settings, public capability directory, and scoped discovery cache. */
export function useAiProviders({ t }) {
  const [settings, setSettings] = useState(null);
  const [loadError, setLoadError] = useState('');
  const [draft, setDraft] = useState(null);
  const [providers, setProviders] = useState(PROVIDER_PRESETS);
  const [modelList, setModelList] = useState(null);
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState(null);
  const [imageConsentOpen, setImageConsentOpen] = useState(false);
  const draftRef = useRef(null);
  const modelRevision = useRef(0);
  const catalogRevision = useRef(0);

  const describe = useCallback((error) => {
    const detail = aiErrorDetail(error);
    return t(aiErrorKey(error)) + (detail ? ` (${detail})` : '');
  }, [t]);

  const load = useCallback(async () => {
    try { setSettings(await getAiSettings()); setLoadError(''); }
    catch (error) { setLoadError(describe(error)); }
  }, [describe]);

  const loadCatalog = useCallback(async (refresh = false) => {
    const revision = ++catalogRevision.current;
    const result = await getAiModelCatalog({ refresh });
    if (revision !== catalogRevision.current) return result;
    if (result?.providers?.length && result.providers.every((p) => p.baseUrl && Array.isArray(p.models))) {
      setProviders(result.providers);
    }
    return result;
  }, []);

  useEffect(() => {
    load();
    loadCatalog().catch(() => {});
    return () => { modelRevision.current += 1; catalogRevision.current += 1; };
  }, [load, loadCatalog]);

  const run = async (name, action) => {
    setBusy(name); setMessage(null);
    try { return await action(); }
    catch (error) { setMessage({ tone: 'error', text: describe(error) }); return undefined; }
    finally { setBusy(''); }
  };

  const replaceDraft = (next) => {
    modelRevision.current += 1;
    draftRef.current = next; setDraft(next); setModelList(null); setImageConsentOpen(false); setMessage(null);
  };
  const updateDraft = (patch) => {
    const current = draftRef.current;
    if (!current) return;
    const endpointChanged = ('baseUrl' in patch && patch.baseUrl !== current.baseUrl)
      || ('kind' in patch && patch.kind !== current.kind);
    const modelChanged = 'model' in patch && patch.model !== current.model;
    if (endpointChanged || 'apiKey' in patch) { modelRevision.current += 1; setModelList(null); }
    const next = { ...current, ...patch,
      ...('apiKey' in patch ? { keyTouched: true } : {}),
      ...('apiKey' in patch ? { imageInput: 'unknown' } : {}),
      ...(endpointChanged ? { allowImages: false, imageInputOverride: 'auto', imageInput: 'unknown' } : {}),
      ...(modelChanged ? { imageInputOverride: 'auto', imageInput: 'unknown' } : {}),
    };
    draftRef.current = next; setDraft(next);
  };

  const readCachedModels = async (next) => {
    const revision = ++modelRevision.current;
    try {
      const result = await discoverAiModels(toInput(next));
      if (revision === modelRevision.current && draftRef.current) setModelList(result);
    } catch { /* Reading a cache never prompts for an unlock or blocks editing. */ }
  };

  const startAdd = () => replaceDraft(emptyDraft());
  const startEdit = (provider) => {
    const preset = findProviderPreset(provider.kind, provider.base_url, providers);
    const next = { ...emptyDraft(), presetId: preset?.id || 'custom',
      customModel: !preset?.models.some((model) => model.id === provider.model), id: provider.id,
      name: provider.name, kind: provider.kind, baseUrl: provider.base_url, model: provider.model,
      hasApiKey: provider.has_api_key, contextTokens: provider.context_tokens ?? defaultContextTokens(provider.base_url),
      contextTokensAuto: provider.context_tokens_auto ?? (provider.context_tokens == null),
      allowImages: provider.allow_images ?? false, imageInputOverride: provider.image_input_override ?? 'auto',
      imageInput: provider.image_input ?? 'unknown' };
    replaceDraft(next); readCachedModels(next);
  };
  const cancelEdit = () => replaceDraft(null);
  const applyPreset = (presetId) => {
    if (presetId === 'custom') { replaceDraft({ ...emptyDraft(), presetId }); return; }
    const preset = providers.find((item) => item.id === presetId);
    if (!preset) return;
    const next = { ...emptyDraft(), presetId, name: preset.name, kind: preset.kind || 'openai_compatible', baseUrl: preset.baseUrl,
      model: preset.models.find((model) => model.recommended)?.id || preset.models[0]?.id || '' };
    replaceDraft(next);
    if (preset.local) readCachedModels(next);
  };

  const preset = providers.find((item) => item.id === draft?.presetId);
  const models = useMemo(() => [...(modelList?.models?.length ? modelList.models : preset?.models ?? [])]
    .sort((a, b) => Number(Boolean(b.recommended)) - Number(Boolean(a.recommended))), [modelList, preset]);
  const selectedModel = models.find((model) => model.id === draft?.model);
  const contextPreset = draft && findProviderPreset(draft.kind, draft.baseUrl, providers);
  const contextModel = (!modelList?.stale && modelList?.models?.find((model) => model.id === draft?.model))
    || contextPreset?.models.find((model) => model.id === draft?.model);
  const contextTokens = draft?.contextTokensAuto ? defaultContextTokens(draft.baseUrl, contextModel) : draft?.contextTokens;
  const imageInput = draft?.imageInputOverride !== 'auto' ? draft?.imageInputOverride
    : draft?.imageInput !== 'unknown' ? draft?.imageInput : selectedModel?.image_input ?? 'unknown';

  const discover = () => run('discover', async () => {
    const revision = ++modelRevision.current;
    const result = await discoverAiModels(toInput(draftRef.current), { refresh: true });
    if (revision !== modelRevision.current) return;
    setModelList(result);
    setMessage({ tone: result.warning ? 'warning' : 'success',
      text: result.warning ? t('settings.ai.models.discovery_failed') : t('settings.ai.models.discovered') });
  });

  const refreshCatalog = () => run('catalog', async () => {
    const result = await loadCatalog(true);
    setMessage({ tone: result?.warning ? 'warning' : 'success',
      text: t(result?.warning ? 'settings.ai.models.catalog_cached' : 'settings.ai.models.catalog_updated') });
    await load();
  });

  const save = () => run('save', async () => {
    const next = await saveAiProvider(toInput(draftRef.current));
    setSettings(next); replaceDraft(null); setMessage({ tone: 'success', text: t('settings.ai.saved') });
  });
  const test = () => run('test', async () => {
    const result = await testAiProvider(toInput(draftRef.current));
    if (!result.ok) { setMessage({ tone: 'error', text: describe(result.error) }); return; }
    const supported = result.tool_calling === 'supported';
    setMessage({ tone: supported ? 'success' : 'warning',
      text: t(supported ? 'settings.ai.test.ok' : 'settings.ai.test.ok_without_tools', { ms: result.latency_ms }) });
    if (draftRef.current?.id) load();
  });
  const testImages = () => run('image-test', async () => {
    const current = draftRef.current;
    const result = await testAiImageInput(toInput(current));
    if (draftRef.current !== current) return;
    updateDraft({ imageInput: result.image_input });
    setMessage({ tone: result.image_input === 'supported' ? 'success' : 'warning',
      text: t(`settings.ai.images.test_${result.image_input}`, { ms: result.latency_ms }) });
    if (current.id) load();
  });

  const setAllowImages = (checked) => {
    if (!checked) { updateDraft({ allowImages: false }); return; }
    let local = false;
    try { local = /^(localhost|127(?:\.\d{1,3}){3}|\[::1\])$/i.test(new URL(draftRef.current.baseUrl).hostname); } catch { /* editable URL */ }
    if (local) updateDraft({ allowImages: true });
    else setImageConsentOpen(true);
  };
  const acceptImageConsent = () => { updateDraft({ allowImages: true }); setImageConsentOpen(false); };

  const remove = (id) => run('delete:' + id, async () => {
    setSettings(await deleteAiProvider(id));
    if (draftRef.current?.id === id) replaceDraft(null);
  });
  const makeDefault = (id) => run('default:' + id, async () => { setSettings(await setDefaultAiProvider(id)); });

  return { settings, loadError, draft, busy, message, providers, models, modelList, imageInput, contextTokens,
    imageConsentOpen, setImageConsentOpen, acceptImageConsent, setAllowImages,
    load, startAdd, startEdit, cancelEdit, updateDraft, applyPreset, save, test, testImages, discover, refreshCatalog, remove, makeDefault };
}
