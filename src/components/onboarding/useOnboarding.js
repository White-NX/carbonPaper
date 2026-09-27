import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { formatError } from '../../lib/errors';
import { notifySettingsChanged, saveAdvancedConfig } from '../../lib/settings_api';
import { completeOnboarding, detectInstalledBrowsers, getOnboardingState } from '../../lib/onboarding_api';
import { dismissAppBoundOffer, getAppBoundStatus, installAppBound } from '../../lib/app_bound_api';
import { useDebugOverlay } from '../overlay/coordinator';
import {
  RERANKER_MODEL_ID,
  RESOURCE_POLICIES,
  WIZARD_STEPS,
  applicableWhatsNew,
  buildTasks,
  featureConfig,
  recommendedChoices,
  selectedBrowsers,
} from './onboardingPlan';
import { useRerankerDownload } from './useRerankerDownload';

// Mirrors useGeneralOptionsController: a stored "custom" choice would keep the
// settings card from showing the policy picked here.
const RESOURCE_POLICY_STORAGE_KEY = 'settings.resourcePolicy';

const STEP_KEYS = {
  features: ['featureMode', 'resourcePolicy'],
  browserStartup: ['browsers', 'launchAtLogin', 'autoStartCapture', 'appBound'],
};

async function loadContext() {
  const [browsers, appBound, models, config] = await Promise.allSettled([
    detectInstalledBrowsers(),
    getAppBoundStatus(),
    invoke('check_model_files'),
    invoke('get_advanced_config'),
  ]);
  const appBoundStatus = appBound.status === 'fulfilled' ? appBound.value : null;
  return {
    browsers: browsers.status === 'fulfilled' ? browsers.value : {},
    appBoundAvailable: Boolean(appBoundStatus?.offer_enable) && appBoundStatus?.reason !== 'repair_required',
    rerankerInstalled: models.status === 'fulfilled' && models.value?.[RERANKER_MODEL_ID]?.complete === true,
    smartClusterEnabled: config.status === 'fulfilled' && Boolean(config.value?.smart_cluster_enabled),
  };
}

/**
 * First-run wizard and "what's new" page: what to show, the user's choices,
 * and applying them.
 *
 * `captureHeld` is true while a new user has not finished the full wizard (and
 * while that is still unknown), so the monitor does not start recording before
 * the user has seen what will be recorded.
 */
export function useOnboarding({ isAuthenticated, requiredModelsReady, requiredDownload, pushNotification, t }) {
  const [state, setState] = useState(null);
  const [stateFailed, setStateFailed] = useState(false);
  const [context, setContext] = useState(null);
  const [choices, setChoices] = useState(null);
  const [step, setStep] = useState(0);
  const [direction, setDirection] = useState('forward');
  const [phase, setPhase] = useState('edit');
  const [taskStatus, setTaskStatus] = useState({});
  const [extensionPath, setExtensionPath] = useState('');
  const [whatsNewSelection, setWhatsNewSelection] = useState({});
  const [whatsNewError, setWhatsNewError] = useState('');
  const [closed, setClosed] = useState(false);
  const [completed, setCompleted] = useState(false);
  const [preview, setPreview] = useState(null);
  const contextRequestedRef = useRef(false);

  const notify = useCallback((notification) => {
    pushNotification?.({ id: `onboarding-${Date.now()}`, timestamp: Date.now(), ...notification });
  }, [pushNotification]);

  const reranker = useRerankerDownload({ ready: isAuthenticated && requiredModelsReady, notify, t });

  useEffect(() => {
    let alive = true;
    getOnboardingState()
      .then((next) => { if (alive) setState(next); })
      .catch((failure) => {
        console.warn('Failed to read onboarding state:', failure);
        if (alive) setStateFailed(true);
      });
    return () => { alive = false; };
  }, []);

  const mode = preview ?? state?.mode ?? null;
  const wantsWizard = mode === 'full' || mode === 'whats_new';

  // Loaded once the user has unlocked: detection results are only needed to
  // prefill choices, and the smart-cluster flag sits behind the session.
  useEffect(() => {
    if (!isAuthenticated || !wantsWizard || contextRequestedRef.current) return;
    contextRequestedRef.current = true;
    loadContext().then((loaded) => {
      setContext(loaded);
      setChoices(recommendedChoices(loaded));
      setWhatsNewSelection(Object.fromEntries(applicableWhatsNew(loaded).map((entry) => [entry.id, true])));
    });
  }, [isAuthenticated, wantsWizard]);

  // A what's-new preview shows every entry, whatever this machine has.
  const effectiveContext = useMemo(() => (
    preview === 'whats_new' && context
      ? { ...context, smartClusterEnabled: false, appBoundAvailable: true }
      : context
  ), [context, preview]);
  const whatsNewEntries = useMemo(
    () => (effectiveContext ? applicableWhatsNew(effectiveContext) : []),
    [effectiveContext],
  );
  const recommended = useMemo(() => (context ? recommendedChoices(context) : null), [context]);
  const tasks = useMemo(
    () => (choices && context ? buildTasks(choices, context) : []),
    [choices, context],
  );

  const finishRecord = useCallback(async () => {
    setCompleted(true);
    if (preview) return;
    try { await completeOnboarding(state?.current_version ?? 1); }
    catch (failure) { console.warn('Failed to record onboarding completion:', failure); }
  }, [preview, state]);

  // Nothing new applies to this user: record the version without asking.
  useEffect(() => {
    if (mode === 'whats_new' && context && whatsNewEntries.length === 0 && !completed) {
      finishRecord();
      setClosed(true);
    }
  }, [mode, context, whatsNewEntries.length, completed, finishRecord]);

  const open = !closed && isAuthenticated && Boolean(context && choices)
    && (mode === 'full' || (mode === 'whats_new' && whatsNewEntries.length > 0));
  const captureHeld = !stateFailed && (state === null || (state.mode === 'full' && !completed));

  // --- Full wizard: choices and navigation -------------------------------

  const setChoice = useCallback((key, value) => {
    setChoices((current) => ({ ...current, [key]: value }));
  }, []);

  const setBrowser = useCallback((browser, on) => {
    setChoices((current) => ({ ...current, browsers: { ...current.browsers, [browser]: on } }));
  }, []);

  const goTo = useCallback((target) => {
    setDirection(target >= step ? 'forward' : 'back');
    setStep(Math.max(0, Math.min(WIZARD_STEPS.length - 1, target)));
  }, [step]);

  const next = useCallback(() => goTo(step + 1), [goTo, step]);
  const back = useCallback(() => goTo(step - 1), [goTo, step]);

  const applyRecommended = useCallback(() => {
    if (!recommended) return;
    setChoices(recommended);
    goTo(WIZARD_STEPS.length - 1);
  }, [goTo, recommended]);

  const resetStep = useCallback((stepId) => {
    if (!recommended) return;
    setChoices((current) => ({
      ...current,
      ...Object.fromEntries(STEP_KEYS[stepId].map((key) => [key, recommended[key]])),
    }));
  }, [recommended]);

  const stepIsRecommended = useCallback((stepId) => (
    Boolean(recommended && choices)
    && STEP_KEYS[stepId].every((key) => JSON.stringify(choices[key]) === JSON.stringify(recommended[key]))
  ), [choices, recommended]);

  // --- Full wizard: applying ---------------------------------------------

  const runners = useMemo(() => ({
    features: async () => {
      const config = featureConfig(choices.featureMode);
      await saveAdvancedConfig({
        classification_enabled: config.classification_enabled,
        // Switched on by the background download when the component lands.
        smart_cluster_enabled: config.smart_cluster_enabled && context.rerankerInstalled,
      });
      notifySettingsChanged(['advanced']);
    },
    resources: async () => {
      const policy = RESOURCE_POLICIES[choices.resourcePolicy];
      await withAuth(() => invoke('toggle_game_mode', { enabled: policy.gameMode }), { autoPrompt: true });
      await withAuth(() => invoke('set_power_saving_enabled', { enabled: policy.powerSaving }), { autoPrompt: true });
      try { localStorage.removeItem(RESOURCE_POLICY_STORAGE_KEY); } catch { /* preference only */ }
      notifySettingsChanged(['advanced', 'power']);
    },
    startup: async () => {
      await withAuth(() => invoke('set_autostart', { enabled: choices.launchAtLogin }), { autoPrompt: true });
      await withAuth(() => invoke('set_monitor_autostart', { enabled: choices.autoStartCapture }), { autoPrompt: true });
      if (context.appBoundAvailable && !choices.appBound && !preview) await dismissAppBoundOffer();
      notifySettingsChanged(['autostart']);
    },
    browsers: async () => {
      let path = '';
      for (const browser of selectedBrowsers(choices)) {
        const result = await withAuth(() => invoke('install_browser_extension', { browser }), { autoPrompt: true });
        if (result?.extension_path) path = result.extension_path;
      }
      setExtensionPath(path);
    },
    reranker: async () => { reranker.queue(); },
    appBound: async () => {
      // Preview builds cannot install it, and a real install restarts the app.
      if (preview) return;
      await installAppBound(true);
    },
  }), [choices, context, preview, reranker]);

  const runTask = useCallback(async (id) => {
    setTaskStatus((current) => ({ ...current, [id]: { status: 'running' } }));
    try {
      await runners[id]();
      setTaskStatus((current) => ({ ...current, [id]: { status: 'done' } }));
      return true;
    } catch (failure) {
      setTaskStatus((current) => ({ ...current, [id]: { status: 'failed', error: formatError(failure) } }));
      return false;
    }
  }, [runners]);

  const apply = useCallback(async () => {
    setPhase('applying');
    for (const task of tasks) {
      if (task.id === 'appBound') continue;
      await runTask(task.id);
    }
    // Recorded before the restart the last step causes, and regardless of a
    // failed step: every failure has a retry here and a home in settings.
    await finishRecord();
    if (tasks.some((task) => task.id === 'appBound')) await runTask('appBound');
    setPhase('applied');
  }, [finishRecord, runTask, tasks]);

  const finish = useCallback(async () => {
    // Anything still downloading continues behind the top bar's progress.
    if (requiredDownload?.modelDownloading) requiredDownload.setIsClosedByUser(true);
    // A declined or failed install is settled here, not offered again at once.
    if (context?.appBoundAvailable && taskStatus.appBound?.status !== 'done' && !preview) {
      dismissAppBoundOffer().catch(() => {});
    }
    setClosed(true);
  }, [context, preview, requiredDownload, taskStatus.appBound]);

  // --- What's new ---------------------------------------------------------

  const setWhatsNewChoice = useCallback((id, on) => {
    setWhatsNewSelection((current) => ({ ...current, [id]: on }));
  }, []);

  const applyWhatsNew = useCallback(async () => {
    setPhase('applying');
    setWhatsNewError('');
    const offered = new Set(whatsNewEntries.map((entry) => entry.id));
    try {
      if (offered.has('smartCluster') && whatsNewSelection.smartCluster) {
        if (effectiveContext.rerankerInstalled) {
          await saveAdvancedConfig({ smart_cluster_enabled: true });
          notifySettingsChanged(['advanced']);
        } else {
          reranker.queue();
        }
      }
      await finishRecord();
      if (offered.has('appBound')) {
        if (whatsNewSelection.appBound && !preview) await installAppBound(true);
        else if (!preview) await dismissAppBoundOffer();
      }
      setClosed(true);
    } catch (failure) {
      setWhatsNewError(formatError(failure));
    } finally {
      setPhase('edit');
    }
  }, [effectiveContext, finishRecord, preview, reranker, whatsNewEntries, whatsNewSelection]);

  const skipWhatsNew = useCallback(async () => {
    await finishRecord();
    if (whatsNewEntries.some((entry) => entry.id === 'appBound') && !preview) {
      dismissAppBoundOffer().catch(() => {});
    }
    setClosed(true);
  }, [finishRecord, preview, whatsNewEntries]);

  const remindLater = useCallback(() => setClosed(true), []);

  // --- Debug preview ------------------------------------------------------

  useDebugOverlay('onboarding', (variant) => {
    setPreview(variant === 'whats_new' ? 'whats_new' : 'full');
    setStep(0);
    setPhase('edit');
    setTaskStatus({});
    setClosed(false);
    if (context) setChoices(recommendedChoices(context));
    if (variant === 'whats_new') setWhatsNewSelection({ smartCluster: true, appBound: true });
  });

  return {
    mode,
    open,
    captureHeld,
    context: effectiveContext,
    choices,
    recommended,
    step,
    stepId: WIZARD_STEPS[step],
    direction,
    phase,
    tasks,
    taskStatus,
    extensionPath,
    reranker,
    whatsNewEntries,
    whatsNewSelection,
    whatsNewError,
    setChoice,
    setBrowser,
    next,
    back,
    goTo,
    applyRecommended,
    resetStep,
    stepIsRecommended,
    apply,
    retryTask: runTask,
    finish,
    setWhatsNewChoice,
    applyWhatsNew,
    skipWhatsNew,
    remindLater,
  };
}
