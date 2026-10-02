import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useAiSearch } from './useAiSearch';
import { getAiSettings, runAiSearch, grantAiRemoteConsent } from '../lib/ai_api';

vi.mock('../lib/ai_api', () => ({
  getAiSettings: vi.fn(), runAiSearch: vi.fn(),
  cancelAiSearch: vi.fn(async () => true), grantAiRemoteConsent: vi.fn(),
}));
const settings = { providers: [{ id: 'p' }], default_provider_id: 'p' };
let finish;
let emit;
beforeEach(() => {
  vi.clearAllMocks();
  getAiSettings.mockResolvedValue(settings);
  grantAiRemoteConsent.mockResolvedValue(settings);
  runAiSearch.mockImplementation(({ onEvent }) => {
    emit = onEvent;
    return new Promise((resolve) => { finish = resolve; });
  });
});
async function setup() {
  const hook = renderHook(() => useAiSearch({ active: true }));
  await waitFor(() => expect(hook.result.current.provider?.id).toBe('p'));
  return hook;
}

describe('AI conversation', () => {
  it('records duration per turn, resets it for follow-ups and freezes completed durations', async () => {
    const { result } = await setup();
    const now = vi.spyOn(performance, 'now').mockReturnValue(1000);
    try {
      act(() => { result.current.start('first'); });
      expect(result.current.elapsedMs).toBeNull();
      now.mockReturnValue(66000);
      await act(async () => finish({ answer: 'Found it.', snapshots: [] }));
      expect(result.current.elapsedMs).toBe(65000);
      now.mockReturnValue(100000);
      act(() => { result.current.start('follow-up'); });
      expect(result.current.elapsedMs).toBeNull();
      expect(result.current.turns[0].elapsedMs).toBe(65000);
      now.mockReturnValue(102000);
      await act(async () => finish({ answer: 'More.', snapshots: [] }));
      expect(result.current.elapsedMs).toBe(2000);
      expect(result.current.turns[0].elapsedMs).toBe(65000);
      act(() => result.current.reset());
      expect(result.current.elapsedMs).toBeNull();
    } finally { now.mockRestore(); }
  });

  it('preserves commentary and reasoning across batched rounds and repeated call IDs', async () => {
    const { result } = await setup();
    act(() => { result.current.start('question'); });
    act(() => {
      emit({ type: 'step_started', step: 1 });
      emit({ type: 'reasoning_delta', text: 'first thought' });
      emit({ type: 'text_delta', text: 'I will search.' });
      emit({ type: 'tool_started', call_id: 'same', name: 'search_nl' });
      emit({ type: 'tool_finished', call_id: 'same', ok: true, item_count: 2 });
      emit({ type: 'step_started', step: 2 });
      emit({ type: 'reasoning_delta', text: 'second thought' });
      emit({ type: 'tool_started', call_id: 'same', name: 'get_snapshot_details' });
      emit({ type: 'tool_finished', call_id: 'same', ok: false });
      emit({ type: 'step_started', step: 3 });
      emit({ type: 'text_delta', text: 'The answer.' });
    });
    await act(async () => finish({ answer: 'The answer.', snapshots: [] }));
    expect(result.current.rounds.map(({ text, reasoning }) => [text, reasoning])).toEqual([
      ['I will search.', 'first thought'], ['', 'second thought'], ['The answer.', ''],
    ]);
    expect(result.current.steps.map((step) => step.status)).toEqual(['done', 'failed']);
    expect(result.current.status).toBe('done');
  });

  it('sends previous completed turns for follow-ups and clears them for a new conversation', async () => {
    const { result } = await setup();
    act(() => { result.current.start('first'); });
    await act(async () => finish({ answer: 'Found [#42]', snapshots: [{ id: 42 }] }));
    act(() => { result.current.start('what about that?'); });
    expect(runAiSearch.mock.lastCall[0]).toMatchObject({
      providerId: 'p', question: 'what about that?', history: [{ question: 'first', answer: 'Found [#42]' }],
    });
    expect(result.current.turns[0].outcome.snapshots).toEqual([{ id: 42 }]);
    await act(async () => finish({ answer: 'More', snapshots: [] }));
    act(() => result.current.reset());
    expect(result.current.turns).toEqual([]);
    act(() => { result.current.start('new'); });
    expect(runAiSearch.mock.lastCall[0].history).toEqual([]);
  });

  it('carries backend time anchors and actual ranges into follow-ups without replaying tools', async () => {
    const { result } = await setup();
    const timeContext = {
      started_at: '2026-09-27T23:59:00+08:00',
      searched_ranges: [{ tool: 'search_ocr_text', start_time: 1790467200000, end_time: 1790553600000 }],
    };
    act(() => { result.current.start('today'); });
    act(() => {
      emit({ type: 'step_started', step: 1 });
      emit({ type: 'reasoning_delta', text: 'working' });
      emit({ type: 'tool_started', call_id: 'a', name: 'search_ocr_text', arguments: { query: 'invoice' } });
      emit({ type: 'tool_finished', call_id: 'a', ok: true, snapshots: [{ id: 42 }] });
    });
    await act(async () => finish({ answer: 'Found [#42]', time_context: timeContext }));
    act(() => { result.current.start('an hour earlier?'); });
    expect(runAiSearch.mock.lastCall[0].history).toEqual([
      { question: 'today', answer: 'Found [#42]', time_context: timeContext },
    ]);
    expect(result.current.turns[0].steps).toHaveLength(1);
  });

  it('keeps a stable bounded history prefix across several follow-ups', async () => {
    const { result } = await setup();
    for (let i = 0; i <= 17; i += 1) {
      act(() => { result.current.start(`question ${i}`); });
      const history = runAiSearch.mock.lastCall[0].history;
      expect(history.length).toBeLessThanOrEqual(12);
      if (i >= 13 && i <= 16) expect(history[0].question).toBe('question 4');
      if (i === 17) expect(history[0].question).toBe('question 8');
      await act(async () => finish({ answer: `answer ${i}`, snapshots: [] }));
    }
    expect(result.current.turns).toHaveLength(17);
  });

  it('stops pending tool indicators and ignores late events after cancellation', async () => {
    const { result } = await setup();
    let reject;
    runAiSearch.mockImplementationOnce(({ onEvent }) => {
      emit = onEvent;
      return new Promise((_, fail) => { reject = fail; });
    });
    act(() => { result.current.start('question'); });
    act(() => {
      emit({ type: 'step_started', step: 1 });
      emit({ type: 'tool_started', call_id: 'a', name: 'search_nl' });
    });
    await act(async () => reject(new Error('AI_CANCELLED')));
    expect(result.current.steps[0].status).toBe('cancelled');
    act(() => emit({ type: 'text_delta', text: 'late' }));
    expect(result.current.answer).toBe('');
  });

  it('retries consent without duplicating conversation history', async () => {
    const { result } = await setup();
    act(() => { result.current.start('first'); });
    await act(async () => finish({ answer: 'answer', snapshots: [] }));
    runAiSearch.mockRejectedValueOnce(new Error('AI_REMOTE_CONSENT_REQUIRED'));
    await act(async () => result.current.start('follow-up'));
    expect(result.current.status).toBe('consent');
    await act(async () => result.current.acceptConsent());
    expect(runAiSearch.mock.lastCall[0].history).toEqual([{ question: 'first', answer: 'answer' }]);
    expect(result.current.turns).toHaveLength(1);
  });
});
