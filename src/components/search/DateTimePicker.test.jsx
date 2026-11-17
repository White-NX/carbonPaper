import React from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { DateTimePicker } from './DateTimePicker';
import { TimeRangeChip } from './SearchFilters';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }),
}));

const dateName = (date) => new Intl.DateTimeFormat('en', { dateStyle: 'full' }).format(new Date(date + 'T12:00'));
const open = () => {
  fireEvent.click(screen.getByRole('button', { name: 'Start' }));
  return within(screen.getByRole('dialog', { name: 'Start' }));
};

describe('DateTimePicker', () => {
  it('applies the selected local date and time only on confirmation and restores focus', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T02:50" onChange={onChange} />);
    const picker = open();
    fireEvent.click(picker.getByRole('gridcell', { name: dateName('2026-02-18') }));
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.hour' }), { target: { value: '03' } });
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.minute' }), { target: { value: '04' } });
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-02-18T03:04');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Start' })).toHaveFocus();
  });

  it.each(['cancel', 'escape', 'outside'])('discards a draft on %s and reopens the saved selection', (action) => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T02:50" onChange={onChange} />);
    const picker = open();
    fireEvent.click(picker.getByRole('gridcell', { name: dateName('2026-02-18') }));
    if (action === 'cancel') fireEvent.click(picker.getByRole('button', { name: 'common.cancel' }));
    else if (action === 'escape') fireEvent.keyDown(document.activeElement, { key: 'Escape' });
    else fireEvent.mouseDown(document.body);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
    expect(open().getByRole('gridcell', { name: dateName('2026-02-17') })).toHaveAttribute('aria-selected', 'true');
  });

  it('navigates across month boundaries with the keyboard before selecting', async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<DateTimePicker label="Start" value="2026-01-31T12:30" onChange={onChange} />);
    open();
    expect(screen.getByRole('gridcell', { name: dateName('2026-01-31') })).toHaveFocus();
    await user.keyboard('{ArrowRight}{Enter}');
    expect(screen.getByRole('gridcell', { name: dateName('2026-02-01') })).toHaveAttribute('aria-selected', 'true');
    expect(onChange).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-02-01T12:30');
  });

  it('allows a leap day when changing the displayed year', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T12:30" onChange={onChange} />);
    const picker = open();
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.year' }), { target: { value: '2024' } });
    fireEvent.click(picker.getByRole('gridcell', { name: dateName('2024-02-29') }));
    fireEvent.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2024-02-29T12:30');
  });

  it('moves from year navigation to the calendar without applying a hidden old selection', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T12:30" onChange={onChange} />);
    const picker = open();
    const year = picker.getByRole('spinbutton', { name: 'dateTimePicker.year' });
    fireEvent.change(year, { target: { value: '2024' } });
    fireEvent.keyDown(year, { key: 'Enter' });
    expect(onChange).not.toHaveBeenCalled();
    expect(picker.getByRole('gridcell', { name: dateName('2024-02-01') })).toHaveFocus();
  });

  it('uses Escape to close the calendar before closing the surrounding range panel', () => {
    const onChange = vi.fn();
    render(<TimeRangeChip preset="custom" startDate="2026-02-17T12:30" endDate="" onChange={onChange} />);
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.custom_active' }));
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.start' }));
    fireEvent.keyDown(document.activeElement, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByTestId('filter-panel')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'advancedSearch.range.start' })).toHaveFocus();
    fireEvent.keyDown(document.activeElement, { key: 'Escape' });
    expect(screen.queryByTestId('filter-panel')).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it('prevents a reversed range while allowing the exact time boundary', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T07:20" max="2026-02-17T08:30"
      invalidMessage="Start must precede end" onChange={onChange} />);
    const picker = open();
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-18') })).toBeDisabled();
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.hour' }), { target: { value: '09' } });
    expect(picker.getByRole('alert')).toHaveTextContent('Start must precede end');
    expect(picker.getByRole('button', { name: 'dateTimePicker.confirm' })).toBeDisabled();
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.hour' }), { target: { value: '08' } });
    fireEvent.change(picker.getByRole('spinbutton', { name: 'dateTimePicker.minute' }), { target: { value: '30' } });
    fireEvent.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-02-17T08:30');
  });

  it('clears an existing date explicitly', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T02:50" onChange={onChange} />);
    fireEvent.click(open().getByRole('button', { name: 'dateTimePicker.clear' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});
