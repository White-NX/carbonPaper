import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import RecapMainlines from './RecapMainlines';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

describe('daily mainline branches', () => {
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
