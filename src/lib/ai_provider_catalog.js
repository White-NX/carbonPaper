// Verified against official provider directories on 2026-09-30.
export const CLOUD_AI_CONTEXT_TOKENS = 128000;

export const PROVIDER_PRESETS = [
  {
    id: 'anthropic', name: 'Claude', kind: 'anthropic', baseUrl: 'https://api.anthropic.com',
    models: [
      { id: 'claude-sonnet-5-5', name: 'Claude Sonnet 5.5', recommended: true },
      { id: 'claude-opus-5-5', name: 'Claude Opus 5.5' },
      { id: 'claude-fable-5-1', name: 'Claude Fable 5.1' },
      { id: 'claude-haiku-4-5-20251001', name: 'Claude Haiku 4.5' },
    ],
  },
  {
    id: 'deepseek', name: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1',
    models: [
      { id: 'deepseek-flash', name: 'DeepSeek V4.1 Flash', recommended: true },
      { id: 'deepseek-v4-pro', name: 'DeepSeek V4 Pro' },
    ],
  },
  {
    id: 'qwen', name: 'Qwen', baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    models: [
      { id: 'qwen3.8-flash', name: 'Qwen 3.8 Flash', recommended: true },
      { id: 'qwen3.7-plus', name: 'Qwen 3.7 Plus' },
      { id: 'qwen3.8-max', name: 'Qwen 3.8 Max' },
    ],
  },
  {
    id: 'kimi', name: 'Kimi', baseUrl: 'https://api.moonshot.cn/v1',
    models: [
      { id: 'kimi-k3', name: 'Kimi K3', recommended: true },
      { id: 'kimi-k2.6', name: 'Kimi K2.6' },
      { id: 'kimi-k2.7-code', name: 'Kimi K2.7 Code' },
      { id: 'kimi-k2.7-code-highspeed', name: 'Kimi K2.7 Code Highspeed' },
    ],
  },
  {
    id: 'glm', name: 'GLM', baseUrl: 'https://open.bigmodel.cn/api/paas/v4',
    models: [
      { id: 'glm-5.3-flash', name: 'GLM-5.3-Flash', recommended: true },
      { id: 'glm-5.3-flashx', name: 'GLM-5.3-FlashX' },
      { id: 'glm-5.3', name: 'GLM-5.3' },
      { id: 'glm-5.2', name: 'GLM-5.2' },
    ],
  },
  {
    id: 'mimo', name: 'MiMo', baseUrl: 'https://api.xiaomimimo.com/v1',
    models: [
      { id: 'mimo-v2.6-flash', name: 'MiMo V2.6 Flash', recommended: true },
      { id: 'mimo-v2.6-pro', name: 'MiMo V2.6 Pro' },
    ],
  },
  {
    id: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1',
    models: [
      { id: 'gpt-6-luna', name: 'GPT-6 Luna', recommended: true },
      { id: 'gpt-6-sol', name: 'GPT-6 Sol' },
    ],
  },
  {
    id: 'openrouter', name: 'OpenRouter', baseUrl: 'https://openrouter.ai/api/v1',
    models: [
      { id: 'deepseek/deepseek-v4.1-flash', name: 'DeepSeek V4.1 Flash', recommended: true },
      { id: 'anthropic/claude-sonnet-5.5', name: 'Claude Sonnet 5.5' },
      { id: 'anthropic/claude-opus-5.5', name: 'Claude Opus 5.5' },
      { id: 'xiaomi/mimo-v2.6-flash', name: 'MiMo V2.6 Flash' },
    ],
  },
  { id: 'ollama', name: 'Ollama', baseUrl: 'http://localhost:11434/v1', local: true, models: [] },
  { id: 'lmstudio', name: 'LM Studio', baseUrl: 'http://localhost:1234/v1', local: true, models: [] },
];

// Identify existing official endpoints without changing saved models or budgets.
// Custom URLs stay editable as custom services, even if their name matches a preset.
export function findProviderPreset(kind, baseUrl) {
  const normalize = (value) => {
    try {
      const url = new URL(value);
      const path = url.pathname.replace(/\/+$/, '').replace(/\/(chat\/completions|messages)$/, '').replace(/\/v1$/, '');
      return `${url.origin}${path}${url.search}`;
    } catch { return value; }
  };
  return PROVIDER_PRESETS.find((preset) => (preset.kind || 'openai_compatible') === kind
    && normalize(preset.baseUrl) === normalize(baseUrl));
}
