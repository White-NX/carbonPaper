import React from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import WorkspaceTimelines from './WorkspaceTimelines';

vi.mock('./Timeline', () => ({
  default: function MockTimeline({ active, compact, onToggleCompact, searchState }) {
    const [time, setTime] = React.useState(0);
    return <div role="region" aria-label={searchState ? 'Search timeline' : 'Records timeline'} hidden={!active}>
      <output aria-label="Viewport time">{time}</output>
      <button onClick={() => setTime((value) => value + 1)}>Move viewport</button>
      <button onClick={onToggleCompact}>{compact ? 'Expand timeline' : 'Collapse timeline'}</button>
    </div>;
  },
}));

beforeEach(() => localStorage.clear());

it('remembers each collapse choice independently and preserves the records camera across navigation', () => {
  const searchState = { markers: [] };
  const { rerender } = render(<WorkspaceTimelines activeTab="preview" searchState={searchState} />);
  const records = within(screen.getByRole('region', { name: 'Records timeline' }));
  fireEvent.click(records.getByRole('button', { name: 'Move viewport' }));
  fireEvent.click(records.getByRole('button', { name: 'Collapse timeline' }));
  expect(localStorage.getItem('recordsTimelineCollapsed')).toBe('true');
  expect(localStorage.getItem('searchTimelineCollapsed')).toBeNull();

  rerender(<WorkspaceTimelines activeTab="advanced-search" searchState={searchState} />);
  fireEvent.click(within(screen.getByRole('region', { name: 'Search timeline' })).getByRole('button', { name: 'Collapse timeline' }));
  expect(localStorage.getItem('searchTimelineCollapsed')).toBe('true');

  rerender(<WorkspaceTimelines activeTab="preview" searchState={searchState} />);
  expect(records.getByLabelText('Viewport time')).toHaveTextContent('1');
  fireEvent.click(records.getByRole('button', { name: 'Expand timeline' }));
  expect(localStorage.getItem('recordsTimelineCollapsed')).toBe('false');
  expect(localStorage.getItem('searchTimelineCollapsed')).toBe('true');
  expect(records.getByLabelText('Viewport time')).toHaveTextContent('1');
});
