import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { RecapTabs } from './RecapTabs';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key }),
}));

describe('RecapTabs', () => {
  it('renders overview and records tabs with correct aria attributes', () => {
    const onChange = vi.fn();
    render(<RecapTabs tab="recap" onChange={onChange} />);

    const recapTab = screen.getByRole('tab', { name: 'recap.overview' });
    const recordsTab = screen.getByRole('tab', { name: 'recap.records' });

    expect(recapTab).toHaveAttribute('aria-selected', 'true');
    expect(recordsTab).toHaveAttribute('aria-selected', 'false');
  });

  it('calls onChange when clicking a tab', () => {
    const onChange = vi.fn();
    render(<RecapTabs tab="recap" onChange={onChange} />);

    fireEvent.click(screen.getByRole('tab', { name: 'recap.records' }));
    expect(onChange).toHaveBeenCalledWith('records');
  });

  it('supports arrow key navigation', () => {
    const onChange = vi.fn();
    render(<RecapTabs tab="recap" onChange={onChange} />);

    const recapTab = screen.getByRole('tab', { name: 'recap.overview' });
    fireEvent.keyDown(recapTab, { key: 'ArrowRight' });
    expect(onChange).toHaveBeenCalledWith('records');

    fireEvent.keyDown(recapTab, { key: 'Home' });
    expect(onChange).toHaveBeenCalledWith('recap');

    fireEvent.keyDown(recapTab, { key: 'End' });
    expect(onChange).toHaveBeenCalledWith('records');
  });
});
