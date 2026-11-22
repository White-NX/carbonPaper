import React from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
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
const rect = (left, top, width, height) => ({ left, top, width, height, right: left + width, bottom: top + height });
const pickTime = (picker, part, value) => fireEvent.click(within(picker.getByRole('listbox', { name: 'dateTimePicker.' + part }))
  .getByRole('option', { name: value.padStart(2, '0') }));

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('DateTimePicker', () => {
  it('selects a local calendar date without time controls and describes highlighted dates', async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<DateTimePicker dateOnly clearable={false} label="Start" value="2026-02-17" min="2026-02-01" max="2026-02-28"
      highlightedDates={['2026-02-17', '2026-02-18']} highlightLabel="Has recap content" onChange={onChange} />);
    expect(screen.getByRole('button', { name: 'Start' })).toHaveTextContent('02/17/2026');
    const picker = open();
    expect(picker.queryByRole('listbox', { name: 'dateTimePicker.hour' })).not.toBeInTheDocument();
    expect(picker.queryByRole('listbox', { name: 'dateTimePicker.minute' })).not.toBeInTheDocument();
    expect(picker.queryByRole('button', { name: 'dateTimePicker.clear' })).not.toBeInTheDocument();
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-17') })).toHaveAttribute('aria-selected', 'true');
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-17') })).toHaveAccessibleDescription('Has recap content');
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-18') })).toHaveAccessibleDescription('Has recap content');
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-19') })).not.toHaveAccessibleDescription('Has recap content');
    expect(picker.getByRole('gridcell', { name: dateName('2026-01-31') })).toBeDisabled();
    expect(picker.getByRole('gridcell', { name: dateName('2026-03-01') })).toBeDisabled();
    await user.click(picker.getByRole('gridcell', { name: dateName('2026-02-18') }));
    expect(onChange).not.toHaveBeenCalled();
    await user.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-02-18');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Start' })).toHaveFocus();
  });

  it('opens beside the range panel and switches sides when the window narrows', () => {
    vi.stubGlobal('innerWidth', 1200);
    vi.stubGlobal('innerHeight', 600);
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function () {
      if (this.dataset.testid === 'filter-panel') return rect(400, 140, 310, 240);
      if (this.getAttribute('role') === 'dialog') return rect(0, 0, 300, 400);
      return rect(444, 315, 252, 28);
    });
    render(<TimeRangeChip preset="custom" startDate="2026-02-17T12:30" endDate="" onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.custom_active' }));
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.start' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveStyle({ left: '716px', top: '140px' });
    expect(screen.getByRole('button', { name: 'advancedSearch.range.end' })).toBeInTheDocument();
    vi.stubGlobal('innerWidth', 800);
    fireEvent(window, new Event('resize'));
    expect(dialog).toHaveStyle({ left: '94px', top: '140px' });
    expect(screen.getByRole('gridcell', { name: dateName('2026-02-17') })).toHaveFocus();
  });

  it('keeps the calendar inside a small viewport when neither side fits', () => {
    vi.stubGlobal('innerWidth', 460);
    vi.stubGlobal('innerHeight', 430);
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function () {
      if (this.dataset.testid === 'filter-panel') return rect(150, 100, 310, 240);
      if (this.getAttribute('role') === 'dialog') return rect(0, 0, 300, 400);
      return rect(194, 275, 252, 28);
    });
    render(<TimeRangeChip preset="custom" startDate="2026-02-17T12:30" endDate="" onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.custom_active' }));
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.range.end' }));
    expect(screen.getByRole('dialog')).toHaveStyle({ left: '146px', top: '8px' });
    expect(screen.getByRole('button', { name: 'dateTimePicker.confirm' })).toBeInTheDocument();
  });

  it('applies the selected local date and time only on confirmation and restores focus', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T02:50" onChange={onChange} />);
    const picker = open();
    fireEvent.click(picker.getByRole('gridcell', { name: dateName('2026-02-18') }));
    pickTime(picker, 'hour', '03');
    pickTime(picker, 'minute', '04');
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

  it('chooses a month with the preset menu keyboard controls and skips disabled months', async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<DateTimePicker label="Start" value="2026-02-17T12:30" min="2026-02-01T00:00" max="2026-03-31T23:59" onChange={onChange} />);
    const picker = open();
    const month = picker.getByRole('button', { name: 'dateTimePicker.month' });
    month.focus();
    await user.keyboard('{ArrowDown}');
    expect(picker.getByRole('menuitemradio', { name: 'February' })).toHaveFocus();
    expect(picker.getByRole('menuitemradio', { name: 'April' })).toBeDisabled();
    await user.keyboard('{End}{Enter}');
    expect(month).toHaveTextContent('March');
    expect(month).toHaveFocus();
    expect(picker.queryByRole('menu')).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(picker.getByRole('gridcell', { name: dateName('2026-03-05') }));
    fireEvent.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-03-05T12:30');
  });

  it('selects time through scrolling and keyboard controls without applying the draft early', async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<DateTimePicker label="Start" value="2026-02-17T02:50" onChange={onChange} />);
    const picker = open();
    const hour = picker.getByRole('listbox', { name: 'dateTimePicker.hour' });
    fireEvent.scroll(hour, { target: { scrollTop: 3 * 32 } });
    expect(within(hour).getByRole('option', { name: '03' })).toHaveAttribute('aria-selected', 'true');
    const minute = picker.getByRole('listbox', { name: 'dateTimePicker.minute' });
    minute.focus();
    await user.keyboard('{End}{ArrowDown}');
    expect(within(minute).getByRole('option', { name: '59' })).toHaveAttribute('aria-selected', 'true');
    await user.keyboard('{Home}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}');
    expect(within(minute).getByRole('option', { name: '04' })).toHaveAttribute('aria-selected', 'true');
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(picker.getByRole('button', { name: 'dateTimePicker.confirm' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('2026-02-17T03:04');
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

  it('dismisses the month menu before the calendar on Escape', async () => {
    const user = userEvent.setup();
    render(<DateTimePicker label="Start" value="2026-02-17T12:30" onChange={vi.fn()} />);
    const picker = open();
    const month = picker.getByRole('button', { name: 'dateTimePicker.month' });
    await user.click(month);
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(month).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Start' })).toHaveFocus();
  });

  it('prevents a reversed range while allowing the exact time boundary', () => {
    const onChange = vi.fn();
    render(<DateTimePicker label="Start" value="2026-02-17T07:20" max="2026-02-17T08:30"
      invalidMessage="Start must precede end" onChange={onChange} />);
    const picker = open();
    expect(picker.getByRole('gridcell', { name: dateName('2026-02-18') })).toBeDisabled();
    pickTime(picker, 'hour', '09');
    expect(picker.getByRole('alert')).toHaveTextContent('Start must precede end');
    expect(picker.getByRole('button', { name: 'dateTimePicker.confirm' })).toBeDisabled();
    pickTime(picker, 'hour', '08');
    pickTime(picker, 'minute', '30');
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
