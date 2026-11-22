import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import RecapMainlines from './RecapMainlines';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

describe('daily mainline branches', () => {
  it('keeps accepted content visible while an update is pending or fails', () => {
    const item = { id: 'saved', title: 'Saved goal', text: 'Previously accepted progress.', task_ids: ['task'], sources: [] };
    const props = { mainlines: { items: [item] }, pending: true, threads: [{ id: 'task', title: 'Saved task' }],
      batches: [{ start_ms: 0, end_ms: 1, activities: [{ task_id: 'task' }] }], time: String, onTask: vi.fn(), onDetails: vi.fn() };
    const { rerender } = render(<RecapMainlines {...props} />);
    expect(screen.getByText(item.text)).toBeVisible();
    expect(screen.getByText('recap.mainlines.updatesPending')).toBeVisible();
    rerender(<RecapMainlines {...props} error="AI_NETWORK_ERROR" />);
    expect(screen.getByText(item.text)).toBeVisible();
    expect(screen.getByText('recap.mainlines.updateFailed')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generationDetails' }));
    expect(props.onDetails).toHaveBeenCalledOnce();
    rerender(<RecapMainlines {...props} pending={false} mainlines={{ items: [{ ...item, text: 'Updated progress.' }] }} />);
    expect(screen.getByText('Updated progress.')).toBeVisible();
    expect(screen.queryByText('recap.mainlines.updatesPending')).not.toBeInTheDocument();
  });
  it('keeps an unverified mainline visible with a warning and no unknown source links', () => {
    const item = { id: 'music', title: 'Music', text: 'Unconfirmed music activity.', task_ids: ['music'], sources: [],
      warnings: [{ field: 'evidence_ids', reason: 'unknown_reference', reference_ids: [99] }] };
    render(<RecapMainlines mainlines={{ items: [item] }} threads={[{ id: 'music', title: 'Music task' }]}
      batches={[{ start_ms: 0, end_ms: 1, activities: [{ task_id: 'music' }] }]} time={String} onTask={vi.fn()} />);
    expect(screen.getByText(item.text)).toBeVisible();
    expect(screen.getByText('recap.unverifiedContent')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'recap.mainlines.sources' })).not.toBeInTheDocument();
  });
  it('shows selectable summaries with task branches and source navigation', () => {
    const sources = [{ id: 42 }];
    const item = { id: 'project', title: 'Improve the project flow', text: 'Reviewed login handling and notification settings.', task_ids: ['login'], sources };
    const onTask = vi.fn();
    const onSources = vi.fn();
    render(<RecapMainlines mainlines={{ items: [item] }} threads={[{ id: 'login', title: 'Review login flow' }]}
      batches={[{ start_ms: 1000, end_ms: 2000, activities: [{ task_id: 'login' }] }, { start_ms: 3000, end_ms: 4000, activities: [{ task_id: 'login' }] }]}
      time={(ms) => `time-${ms}`} onTask={onTask} onSources={onSources} />);
    const title = screen.getByRole('heading', { name: item.title });
    const summary = screen.getByText(item.text);
    const task = screen.getByRole('button', { name: /Review login flow/ });
    expect(title.tagName).toBe('H3');
    expect(summary.closest('button')).toBeNull();
    expect(screen.queryByRole('button', { name: /Improve the project flow/ })).not.toBeInTheDocument();
    expect(screen.queryByText('recap.mainlines.expandSummary')).not.toBeInTheDocument();
    expect(screen.queryByText('recap.mainlines.collapseSummary')).not.toBeInTheDocument();
    expect(task).toBeVisible();
    expect(task).toHaveTextContent('time-1000–time-2000 · time-3000–time-4000');
    fireEvent.click(task);
    expect(onTask).toHaveBeenCalledWith('login', 'mainline:project');
    fireEvent.click(screen.getByRole('button', { name: 'recap.mainlines.sources' }));
    expect(onSources).toHaveBeenCalledWith(sources);
  });
});
