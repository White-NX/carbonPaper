import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

import { useAiEmbeddingController } from './useAiEmbeddingController';

const loadFilterConfig = vi.fn(async () => {});
const loadSpacyModels = vi.fn(async () => {});
const { eventListeners } = vi.hoisted(() => ({ eventListeners: new Map() }));

vi.mock('../../lib/auth_api', () => ({
  withAuth: vi.fn((fn) => fn()),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (eventName, handler) => {
    eventListeners.set(eventName, handler);
    return vi.fn();
  }),
}));

vi.mock('./agent-access/useSensitiveFilterSettings', () => ({
  useSensitiveFilterSettings: () => ({
    loadFilterConfig,
    loadSpacyModels,
    filterEnabled: true,
    filterCategories: [],
    filterMode: 'reject',
    filterLevel: 'standard',
    showAdvanced: false,
    setShowAdvanced: vi.fn(),
    piiEnabled: true,
    piiEntities: [],
    spacyModels: [],
    downloadingModel: null,
    recheckLoading: false,
    showPiiAdvanced: false,
    setShowPiiAdvanced: vi.fn(),
  }),
}));

const smokeReport = {
  ok: true,
  failure_kind: null,
  runtime_generation: 7,
  tool_schema_version: 2,
  expected_tool_count: 12,
  advertised_tool_count: 12,
  stages: [
    { id: 'initialize', status: 'passed', duration_ms: 1 },
    { id: 'ping', status: 'passed', duration_ms: 2 },
    { id: 'tools_list', status: 'passed', duration_ms: 3 },
    { id: 'metadata_query', status: 'passed', duration_ms: 4 },
  ],
};

const status = {
  enabled: true,
  port: 24567,
  active_port: 24567,
  port_consistent: true,
  runtime_generation: 7,
  running: true,
  state: 'running',
  privacy_acknowledged: true,
  server_version: '0.8.4',
  skill: {
    id: 'carbonpaper-memory',
    source_repository: 'https://example.test/carbonPaperSkill',
    tool_schema_version: 2,
  },
  capabilities: {
    search_nl: true,
    search_nl_backend: 'rust',
  },
};

let statusResponse;
let smokeResponse;
let resetTokenError;
let setEnabledError;

function t(key, values = {}) {
  if (key.endsWith('.variants.cursor.prompt')) {
    return `Cursor ${values.skillName} ${values.repo} ${values.endpoint} schema ${values.toolSchemaVersion} Authorization: Bearer <CarbonPaper token>`;
  }
  return key;
}

describe('useAiEmbeddingController Agent onboarding', () => {
  let writeText;

  beforeEach(() => {
    localStorage.clear();
    eventListeners.clear();
    statusResponse = { ...status };
    smokeResponse = { ...smokeReport };
    resetTokenError = null;
    setEnabledError = null;
    writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    });
    invoke.mockImplementation(async (command) => {
      if (command === 'mcp_get_status') return statusResponse;
      if (command === 'mcp_run_smoke_test') return smokeResponse;
      if (command === 'mcp_reset_token') {
        if (resetTokenError) throw resetTokenError;
        return { copied_to_clipboard: true };
      }
      if (command === 'mcp_set_enabled') {
        if (setEnabledError) throw setEnabledError;
        return { port: statusResponse.port };
      }
      return undefined;
    });
  });

  it('uses backend Skill metadata and copies a token-free Agent-specific prompt', async () => {
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.agentSkill).toEqual(status.skill);

    act(() => result.current.handleAgentVariantChange('cursor'));
    await act(async () => result.current.handleCopyAgentSetupPrompt());

    const prompt = writeText.mock.calls.at(-1)[0];
    expect(prompt).toContain('Cursor carbonpaper-memory');
    expect(prompt).toContain('http://127.0.0.1:24567/mcp');
    expect(prompt).toContain('schema 2');
    expect(prompt).toContain('<CarbonPaper token>');
    expect(prompt).not.toContain('mcp_token_encrypted');
    expect(localStorage.getItem('mcpAgentSetupVariant')).toBe('cursor');
  });

  it('stores the sanitized smoke report and includes it in copied diagnostics', async () => {
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.handleRunMcpSmokeTest());
    expect(invoke).toHaveBeenCalledWith('mcp_run_smoke_test');
    expect(result.current.smokeTestReport).toEqual(smokeReport);

    await act(async () => result.current.handleCopyAgentDiagnostics());
    const diagnostics = JSON.parse(writeText.mock.calls.at(-1)[0]);
    expect(diagnostics.mcp_endpoint).toBe('http://127.0.0.1:24567/mcp');
    expect(diagnostics.skill.tool_schema_version).toBe(2);
    expect(diagnostics.smoke_test).toEqual(smokeReport);
    expect(JSON.stringify(diagnostics)).not.toContain('Bearer');
  });

  it('keeps a current port-mismatch failure visible without treating the service as healthy', async () => {
    const mismatchReport = {
      ...smokeReport,
      ok: false,
      failure_kind: 'port_mismatch',
      advertised_tool_count: null,
      stages: smokeReport.stages.map((stage) => ({ ...stage, status: 'skipped', duration_ms: 0 })),
    };
    smokeResponse = mismatchReport;
    statusResponse = {
      ...statusResponse,
      active_port: 24568,
      port_consistent: false,
      state: 'error',
    };
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.handleRunMcpSmokeTest());

    expect(result.current.running).toBe(false);
    expect(result.current.smokeTestReport).toEqual(mismatchReport);
  });

  it('clears a passed report as soon as the service status changes', async () => {
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.handleRunMcpSmokeTest());
    expect(result.current.smokeTestReport).toEqual(smokeReport);
    await waitFor(() => expect(eventListeners.has('mcp-status-changed')).toBe(true));

    statusResponse = {
      ...statusResponse,
      enabled: false,
      active_port: null,
      port_consistent: true,
      runtime_generation: 8,
      running: false,
      state: 'disabled',
    };
    act(() => eventListeners.get('mcp-status-changed')({ payload: { state: 'disabled' } }));

    await waitFor(() => expect(result.current.smokeTestReport).toBeNull());
    await waitFor(() => expect(result.current.running).toBe(false));
  });

  it('clears a passed report even when token rotation or startup fails', async () => {
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.handleRunMcpSmokeTest());
    resetTokenError = new Error('rotation failed');
    await act(async () => result.current.handleResetToken());
    expect(result.current.smokeTestReport).toBeNull();

    resetTokenError = null;
    await act(async () => result.current.handleRunMcpSmokeTest());
    expect(result.current.smokeTestReport).toEqual(smokeReport);
    setEnabledError = new Error('bind failed');
    await act(async () => result.current.startMcpService({ auto: false }));
    expect(result.current.smokeTestReport).toBeNull();
  });

  it('omits a report from diagnostics when its runtime generation is stale', async () => {
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.handleRunMcpSmokeTest());
    statusResponse = { ...statusResponse, runtime_generation: 8 };

    await act(async () => result.current.handleCopyAgentDiagnostics());

    const diagnostics = JSON.parse(writeText.mock.calls.at(-1)[0]);
    expect(diagnostics.smoke_test).toBeNull();
    expect(result.current.smokeTestReport).toBeNull();
  });

  it('blocks lifecycle operations while a smoke test is in flight', async () => {
    let resolveSmoke;
    smokeResponse = new Promise((resolve) => {
      resolveSmoke = resolve;
    });
    const { result } = renderHook(() => useAiEmbeddingController({ t }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    let smokePromise;
    act(() => {
      smokePromise = result.current.handleRunMcpSmokeTest();
    });
    await waitFor(() => expect(result.current.mcpOperationLoading).toBe(true));

    await act(async () => {
      expect(await result.current.startMcpService({ auto: false })).toBeNull();
      await result.current.handleToggle();
      await result.current.handleResetToken();
    });
    expect(invoke).not.toHaveBeenCalledWith('mcp_set_enabled', expect.anything());
    expect(invoke).not.toHaveBeenCalledWith('mcp_reset_token');

    await act(async () => {
      resolveSmoke(smokeReport);
      await smokePromise;
    });
    expect(result.current.mcpOperationLoading).toBe(false);
    expect(result.current.smokeTestReport).toEqual(smokeReport);
  });
});
