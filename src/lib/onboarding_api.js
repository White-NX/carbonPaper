import { invoke } from '@tauri-apps/api/core';

/** `{ mode: 'full' | 'whats_new' | 'none', current_version, completed_version }` */
export const getOnboardingState = () => invoke('get_onboarding_state');

/** Record that the wizard for `version` was finished. */
export const completeOnboarding = (version) => invoke('complete_onboarding', { version });

/** `{ chrome: boolean, edge: boolean }` for browsers found in their usual locations. */
export const detectInstalledBrowsers = () => invoke('detect_installed_browsers');
