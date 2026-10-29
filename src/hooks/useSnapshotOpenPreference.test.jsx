import { act, renderHook } from '@testing-library/react';
import { beforeEach, expect, it } from 'vitest';
import { useSnapshotOpenPreference } from './useSnapshotOpenPreference';
import { setPreference } from '../lib/preference_store';

beforeEach(() => localStorage.clear());
it.each(['search', 'clusters', 'recap'])('defaults %s screenshots to the current page and honors saved window choices', (scope) => {
  const { result } = renderHook(() => useSnapshotOpenPreference(scope));
  expect(result.current).toBe(false);
  act(() => setPreference(`cardClickBehavior_${scope}`, 'standalone'));
  expect(result.current).toBe(true);
  act(() => setPreference(`cardClickBehavior_${scope}`, 'preview'));
  expect(result.current).toBe(false);
});
