import { usePreference } from '../lib/preference_store';

/** All screenshot sources share the contextual default; saved window choices still apply. */
export function useSnapshotOpenPreference(scope) {
  return usePreference(`cardClickBehavior_${scope}`, 'preview') === 'standalone';
}
