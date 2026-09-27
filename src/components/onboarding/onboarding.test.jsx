import React from 'react';
import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { completeOnboarding, detectInstalledBrowsers, getOnboardingState } from '../../lib/onboarding_api';
import { dismissAppBoundOffer, getAppBoundStatus, installAppBound } from '../../lib/app_bound_api';
import { applicableWhatsNew, buildTasks, recommendedChoices } from './onboardingPlan';
import { useOnboarding } from './useOnboarding';
import OnboardingOverlay from './OnboardingOverlay';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key, i18n: { language: 'zh-CN' } }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('../../i18n', () => ({ changeAppLanguage: vi.fn() }));
vi.mock('../../lib/auth_api', () => ({ withAuth: (fn) => fn() }));
vi.mock('../../lib/onboarding_api', () => ({
  getOnboardingState: vi.fn(),
  completeOnboarding: vi.fn(),
  detectInstalledBrowsers: vi.fn(),
}));
vi.mock('../../lib/app_bound_api', () => ({
  getAppBoundStatus: vi.fn(),
  dismissAppBoundOffer: vi.fn(),
  installAppBound: vi.fn(),
}));

const t = (key) => key;

function mockBackend({ mode = 'full', appBound = false, reranker = false, smartCluster = false } = {}) {
  getOnboardingState.mockResolvedValue({ mode, current_version: 1, completed_version: 0 });
  completeOnboarding.mockResolvedValue(undefined);
  detectInstalledBrowsers.mockResolvedValue({ chrome: false, edge: true });
  getAppBoundStatus.mockResolvedValue({ offer_enable: appBound });
  dismissAppBoundOffer.mockResolvedValue(undefined);
  installAppBound.mockResolvedValue(undefined);
  invoke.mockImplementation(async (command) => {
    if (command === 'check_model_files') return { 'bge-reranker-v2-m3': { complete: reranker } };
    if (command === 'get_advanced_config') return { smart_cluster_enabled: smartCluster };
    if (command === 'install_browser_extension') return { extension_path: 'C:\\ext' };
    return undefined;
  });
}

function renderOnboarding(props = {}) {
  return renderHook(() => useOnboarding({
    isAuthenticated: true,
    requiredModelsReady: false,
    requiredDownload: { modelDownloading: false, setIsClosedByUser: vi.fn() },
    pushNotification: vi.fn(),
    t,
    ...props,
  }));
}

const calls = (command) => invoke.mock.calls.filter(([name]) => name === command).map(([, args]) => args);

beforeEach(() => {
  localStorage.clear();
});

describe('onboarding plan', () => {
  it('recommends smart features, protected performance and detected browsers', () => {
    expect(recommendedChoices({ browsers: { edge: true }, appBoundAvailable: true })).toEqual({
      featureMode: 'smart',
      resourcePolicy: 'performance',
      browsers: { chrome: false, edge: true },
      launchAtLogin: true,
      autoStartCapture: true,
      appBound: true,
    });
  });

  it('queues the smart component in the background and restarts last', () => {
    const choices = recommendedChoices({ browsers: { chrome: true }, appBoundAvailable: true });
    const tasks = buildTasks(choices, { rerankerInstalled: false, appBoundAvailable: true });
    expect(tasks.map((task) => task.id)).toEqual(['features', 'resources', 'startup', 'browsers', 'reranker', 'appBound']);
    expect(tasks.find((task) => task.id === 'reranker')).toMatchObject({ background: true, sizeMb: 560 });
  });

  it('leaves out steps with nothing to do', () => {
    const choices = { ...recommendedChoices(), featureMode: 'basic' };
    expect(buildTasks(choices, { rerankerInstalled: false }).map((task) => task.id))
      .toEqual(['features', 'resources', 'startup']);
  });

  it('offers only the new features that would change something', () => {
    expect(applicableWhatsNew({ smartClusterEnabled: true, appBoundAvailable: false })).toEqual([]);
    expect(applicableWhatsNew({ smartClusterEnabled: false, appBoundAvailable: true }).map((entry) => entry.id))
      .toEqual(['smartCluster', 'appBound']);
  });
});

describe('useOnboarding', () => {
  it('holds capture until a new user finishes, then applies the recommended settings', async () => {
    mockBackend();
    const { result } = renderOnboarding();
    expect(result.current.captureHeld).toBe(true);
    await waitFor(() => expect(result.current.open).toBe(true));

    act(() => result.current.applyRecommended());
    expect(result.current.stepId).toBe('review');
    await act(() => result.current.apply());

    expect(result.current.phase).toBe('applied');
    expect(result.current.captureHeld).toBe(false);
    expect(completeOnboarding).toHaveBeenCalledWith(1);
    // The smart feature waits for its component, which downloads in the background.
    expect(calls('set_advanced_config')).toContainEqual({ config: { classification_enabled: true, smart_cluster_enabled: false } });
    expect(result.current.reranker.status).toBe('queued');
    expect(calls('toggle_game_mode')).toEqual([{ enabled: true }]);
    expect(calls('set_power_saving_enabled')).toEqual([{ enabled: true }]);
    expect(calls('set_autostart')).toEqual([{ enabled: true }]);
    expect(calls('set_monitor_autostart')).toEqual([{ enabled: true }]);
    expect(calls('install_browser_extension')).toEqual([{ browser: 'edge' }]);
    expect(result.current.extensionPath).toBe('C:\\ext');
    expect(installAppBound).not.toHaveBeenCalled();
  });

  it('records completion before the install that restarts the app', async () => {
    mockBackend({ appBound: true });
    const order = [];
    completeOnboarding.mockImplementation(async () => { order.push('complete'); });
    installAppBound.mockImplementation(async () => { order.push('appBound'); });
    const { result } = renderOnboarding();
    await waitFor(() => expect(result.current.open).toBe(true));

    act(() => result.current.applyRecommended());
    await act(() => result.current.apply());
    expect(order).toEqual(['complete', 'appBound']);
  });

  it('keeps a failed step retryable without blocking the rest', async () => {
    mockBackend();
    const { result } = renderOnboarding();
    await waitFor(() => expect(result.current.open).toBe(true));
    const fallback = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, args) => {
      if (command === 'set_autostart') throw new Error('denied');
      return fallback(command, args);
    });

    act(() => result.current.applyRecommended());
    await act(() => result.current.apply());
    expect(result.current.taskStatus.startup).toEqual({ status: 'failed', error: 'denied' });
    expect(result.current.taskStatus.browsers.status).toBe('done');
    expect(completeOnboarding).toHaveBeenCalled();

    invoke.mockImplementation(fallback);
    await act(() => result.current.retryTask('startup'));
    expect(result.current.taskStatus.startup.status).toBe('done');
  });

  it('settles a what\'s-new version silently when nothing new applies', async () => {
    mockBackend({ mode: 'whats_new', smartCluster: true });
    const { result } = renderOnboarding();
    await waitFor(() => expect(completeOnboarding).toHaveBeenCalledWith(1));
    expect(result.current.open).toBe(false);
    expect(result.current.captureHeld).toBe(false);
  });

  it('turns on the smart feature from the what\'s-new page', async () => {
    mockBackend({ mode: 'whats_new', reranker: true });
    const { result } = renderOnboarding();
    await waitFor(() => expect(result.current.open).toBe(true));
    expect(result.current.whatsNewEntries.map((entry) => entry.id)).toEqual(['smartCluster']);

    await act(() => result.current.applyWhatsNew());
    expect(calls('set_advanced_config')).toContainEqual({ config: { smart_cluster_enabled: true } });
    expect(completeOnboarding).toHaveBeenCalledWith(1);
    expect(result.current.open).toBe(false);
  });

  it('does not hold capture for a user who has finished before', async () => {
    mockBackend({ mode: 'none' });
    const { result } = renderOnboarding();
    await waitFor(() => expect(result.current.captureHeld).toBe(false));
    expect(result.current.open).toBe(false);
  });
});

describe('OnboardingWizard', () => {
  function Harness() {
    const onboarding = useOnboarding({
      isAuthenticated: true,
      requiredModelsReady: false,
      requiredDownload: { modelDownloading: true, setIsClosedByUser: vi.fn() },
      pushNotification: vi.fn(),
      t,
    });
    return (
      <OnboardingOverlay
        onboarding={onboarding}
        required={{ needDownload: true, sizeMb: 323, progress: 40, error: null, retry: vi.fn() }}
      />
    );
  }

  it('goes from welcome to review with one click and finishes there', async () => {
    mockBackend();
    render(<Harness />);
    fireEvent.click(await screen.findByRole('button', { name: 'onboarding.welcome.recommended.action' }));
    expect(screen.getByText('onboarding.headers.review.title')).toBeInTheDocument();
    expect(screen.getByText('onboarding.review.requiredComponents')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'onboarding.actions.finish' }));
    fireEvent.click(await screen.findByRole('button', { name: 'onboarding.actions.start' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('walks through the custom steps and moves between options with arrow keys', async () => {
    mockBackend();
    render(<Harness />);
    fireEvent.click(await screen.findByRole('button', { name: 'onboarding.welcome.custom.action' }));
    const smart = screen.getByRole('radio', { name: /onboarding\.featureModes\.smart\.label/ });
    expect(smart).toHaveAttribute('aria-checked', 'true');
    // "Restore recommended" appears only once something differs from it.
    expect(screen.queryByRole('button', { name: 'onboarding.actions.resetStep' })).not.toBeInTheDocument();

    fireEvent.keyDown(smart, { key: 'ArrowLeft' });
    expect(screen.getByRole('radio', { name: /onboarding\.featureModes\.basic\.label/ })).toHaveAttribute('aria-checked', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'onboarding.actions.resetStep' }));
    expect(screen.getByRole('radio', { name: /onboarding\.featureModes\.smart\.label/ })).toHaveAttribute('aria-checked', 'true');
    expect(screen.queryByRole('button', { name: 'onboarding.actions.resetStep' })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'onboarding.actions.next' }));
    expect(screen.getByText('onboarding.headers.browserStartup.title')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'onboarding.actions.back' }));
    expect(screen.getByText('onboarding.headers.features.title')).toBeInTheDocument();
  });
});
