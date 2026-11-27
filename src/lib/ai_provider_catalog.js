import presets from './ai_provider_presets.json';
import { DEFAULT_AI_CONTEXT_TOKENS, LOCAL_AI_CONTEXT_TOKENS, MAX_AI_CONTEXT_TOKENS } from './ai_api';

export const PROVIDER_PRESETS = presets.providers;

export function defaultContextTokens(baseUrl, model) {
  if (Number.isInteger(model?.context_tokens) && model.context_tokens > 0) {
    return Math.min(model.context_tokens, MAX_AI_CONTEXT_TOKENS);
  }
  try {
    if (/^(localhost|127(?:\.\d{1,3}){3}|\[::1\])$/i.test(new URL(baseUrl).hostname)) {
      return LOCAL_AI_CONTEXT_TOKENS;
    }
  } catch { /* The connection address can still be edited. */ }
  return DEFAULT_AI_CONTEXT_TOKENS;
}

// Identify exact endpoints while keeping saved model IDs and user budgets.
export function findProviderPreset(kind, baseUrl, providers = PROVIDER_PRESETS) {
  const normalize = (value, providerKind) => {
    try {
      const url = new URL(value);
      let path = url.pathname.replace(/\/+$/, '').replace(/\/(chat\/completions|messages|models)$/, '');
      if (providerKind === 'anthropic' && !path.endsWith('/v1')) path += '/v1';
      return `${url.origin}${path}/models${url.search}`;
    } catch { return value; }
  };
  return providers.find((preset) => (preset.kind || 'openai_compatible') === kind
    && normalize(preset.baseUrl, kind) === normalize(baseUrl, kind));
}
