import { useCallback, useEffect, useRef, useState } from 'react';
import { cancelAiSearch, getAiSettings, grantAiRemoteConsent, runAiSearch } from '../lib/ai_api';

let nextRequest = 0;

/**
 * State for one AI search at a time.
 *
 * `status` is `idle`, `running`, `done`, `cancelled`, `error`, or `consent`
 * (the default service is remote and the user has not yet agreed to send
 * screenshot text to it). `steps` lists tool calls in the order they ran.
 */
export function useAiSearch({ active }) {
  const [settings, setSettings] = useState(null);
  const [status, setStatus] = useState('idle');
  const [question, setQuestion] = useState('');
  const [answer, setAnswer] = useState('');
  const [steps, setSteps] = useState([]);
  const [rounds, setRounds] = useState([]);
  const [turns, setTurns] = useState([]);
  const [elapsedMs, setElapsedMs] = useState(null);
  const roundRef = useRef(0);
  const [outcome, setOutcome] = useState(null);
  const [error, setError] = useState(null);
  const requestRef = useRef(null);
  const pendingQuestion = useRef('');

  const reloadSettings = useCallback(async () => {
    try { setSettings(await getAiSettings()); } catch { setSettings({ providers: [] }); }
  }, []);

  // Settings can change in the settings window, so re-read on every visit.
  useEffect(() => { if (active) reloadSettings(); }, [active, reloadSettings]);

  const provider = settings?.providers?.find((p) => p.id === settings.default_provider_id) ?? null;

  const handleEvent = useCallback((requestId, event) => {
    if (requestRef.current !== requestId) return;
    const roundId = event.type === 'step_started' ? event.step : roundRef.current;
    switch (event.type) {
      case 'step_started':
        roundRef.current = event.step;
        setRounds((current) => [...current, { id: event.step, text: '', reasoning: '' }]);
        setAnswer('');
        break;
      case 'reasoning_delta':
        setRounds((current) => current.map((round) => round.id === roundId
          ? { ...round, reasoning: round.reasoning + event.text } : round));
        break;
      case 'text_delta':
        setRounds((current) => current.map((round) => round.id === roundId
          ? { ...round, text: round.text + event.text } : round));
        setAnswer((current) => current + event.text);
        break;
      case 'tool_started':
        setSteps((current) => [...current, { id: `${roundId}:${event.call_id}`, callId: event.call_id, round: roundId, name: event.name, arguments: event.arguments, status: 'running' }]);
        break;
      case 'tool_finished':
        setSteps((current) => current.map((step) => (step.callId === event.call_id && step.round === roundId
          ? { ...step, status: event.ok ? 'done' : 'failed', itemCount: event.item_count, snapshots: event.snapshots }
          : step)));
        break;
      default:
        break;
    }
  }, []);

  const start = useCallback(async (text) => {
    const trimmed = text.trim();
    if (!trimmed || requestRef.current) return;
    const requestId = `ai-${Date.now()}-${++nextRequest}`;
    const startedAt = performance.now();
    requestRef.current = requestId;
    const previous = ['done', 'cancelled', 'error'].includes(status) && question
      ? [...turns, { question, answer, steps, rounds, outcome, status, error, elapsedMs }] : turns;
    setTurns(previous);
    roundRef.current = 0;
    setRounds([]);
    setQuestion(trimmed);
    setAnswer('');
    setSteps([]);
    setOutcome(null);
    setError(null);
    setElapsedMs(null);
    setStatus('running');
    try {
      const completed = previous.filter((turn) => turn.status === 'done');
      // Match the backend's four-turn eviction chunks, retaining the same
      // oldest context over several follow-ups instead of shifting each time.
      const historyStart = Math.ceil(Math.max(0, completed.length - 12) / 4) * 4;
      const result = await runAiSearch({ requestId, question: trimmed, providerId: provider?.id,
        history: completed.slice(historyStart).map((turn) => ({
          question: turn.question, answer: turn.answer,
          ...(turn.outcome?.time_context ? { time_context: turn.outcome.time_context } : {}),
          ...(turn.outcome?.messages ? { messages: turn.outcome.messages } : {}),
        })),
        onEvent: (event) => handleEvent(requestId, event) });
      if (requestRef.current !== requestId) return;
      setOutcome(result);
      setAnswer(result.answer);
      setRounds((current) => current.map((round, index) => index === current.length - 1
        ? { ...round, text: result.answer } : round));
      setStatus('done');
    } catch (err) {
      if (requestRef.current !== requestId) return;
      const message = String(err?.message ?? err);
      setSteps((current) => current.map((step) => step.status === 'running'
        ? { ...step, status: message.includes('AI_CANCELLED') ? 'cancelled' : 'failed' } : step));
      if (message.includes('AI_CANCELLED')) setStatus('cancelled');
      else if (message.includes('AI_REMOTE_CONSENT_REQUIRED')) {
        pendingQuestion.current = trimmed;
        setStatus('consent');
      } else {
        setError(err);
        setStatus('error');
      }
    } finally {
      if (requestRef.current === requestId) {
        setElapsedMs(Math.max(0, performance.now() - startedAt));
        requestRef.current = null;
      }
    }
  }, [handleEvent, status, question, answer, steps, rounds, outcome, error, turns, elapsedMs, provider?.id]);

  const reset = useCallback(() => {
    if (requestRef.current) return;
    setTurns([]); setRounds([]); setQuestion(''); setAnswer('');
    setSteps([]); setOutcome(null); setError(null); setStatus('idle');
    setElapsedMs(null);
  }, []);

  const cancel = useCallback(() => {
    const requestId = requestRef.current;
    if (requestId) cancelAiSearch(requestId).catch(() => {});
  }, []);

  const acceptConsent = useCallback(async () => {
    try {
      setSettings(await grantAiRemoteConsent());
    } catch (err) {
      setError(err);
      setStatus('error');
      return;
    }
    start(pendingQuestion.current);
  }, [start]);

  const declineConsent = useCallback(() => setStatus('idle'), []);

  // Leaving the page stops the run instead of letting it spend tokens unseen.
  useEffect(() => () => {
    const requestId = requestRef.current;
    if (requestId) cancelAiSearch(requestId).catch(() => {});
  }, []);

  return {
    settings, provider, status, question, answer, steps, rounds, turns, outcome, error, elapsedMs,
    start, reset, cancel, acceptConsent, declineConsent, reloadSettings,
  };
}
