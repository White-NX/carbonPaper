import React from 'react';
import { act, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setPreference, subscribePreference, usePreference } from './preference_store';

beforeEach(() => localStorage.clear());
function Reader() { return <span>{usePreference('cardClickBehavior_search', 'preview')}</span>; }

describe('shared window preferences', () => {
  it('updates consumers for both local edits and changes from another window', () => {
    render(<Reader />);
    expect(screen.getByText('preview')).toBeInTheDocument();
    act(() => setPreference('cardClickBehavior_search', 'standalone'));
    expect(screen.getByText('standalone')).toBeInTheDocument();
    act(() => {
      localStorage.setItem('cardClickBehavior_search', 'preview');
      window.dispatchEvent(new StorageEvent('storage', { key: 'cardClickBehavior_search', newValue: 'preview' }));
    });
    expect(screen.getByText('preview')).toBeInTheDocument();
  });
  it('isolates keys and removes subscriptions when the consumer closes', () => {
    const listener = vi.fn();
    const unsubscribe = subscribePreference('theme', listener);
    setPreference('language', 'en');
    expect(listener).not.toHaveBeenCalled();
    setPreference('theme', 'dark');
    expect(listener).toHaveBeenCalledTimes(1);
    unsubscribe();
    setPreference('theme', 'light');
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
