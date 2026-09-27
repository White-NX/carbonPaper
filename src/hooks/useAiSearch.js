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
    switch (event.type) {
      case 'step_started':
        // Text from an earlier step was commentary before a tool call.
        setAnswer('');
        break;
      case 'text_delta':
        setAnswer((current) => current + event.text);
        break;
      case 'tool_started':
        setSteps((current) => [...current, { id: event.call_id, name: event.name, arguments: event.arguments, status: 'running' }]);
        break;
      case 'tool_finished':
        setSteps((current) => current.map((step) => (step.id === event.call_id
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
    requestRef.current = requestId;
    setQuestion(trimmed);
    setAnswer('');
    setSteps([]);
    setOutcome(null);
    setError(null);
    setStatus('running');
    try {
      const result = await runAiSearch({ requestId, question: trimmed, onEvent: (event) => handleEvent(requestId, event) });
      if (requestRef.current !== requestId) return;
      setOutcome(result);
      setAnswer(result.answer);
      setStatus('done');
    } catch (err) {
      if (requestRef.current !== requestId) return;
      const message = String(err?.message ?? err);
      if (message.includes('AI_CANCELLED')) setStatus('cancelled');
      else if (message.includes('AI_REMOTE_CONSENT_REQUIRED')) {
        pendingQuestion.current = trimmed;
        setStatus('consent');
      } else {
        setError(err);
        setStatus('error');
      }
    } finally {
      if (requestRef.current === requestId) requestRef.current = null;
    }
  }, [handleEvent]);

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
    settings, provider, status, question, answer, steps, outcome, error,
    start, cancel, acceptConsent, declineConsent, reloadSettings,
  };
}
