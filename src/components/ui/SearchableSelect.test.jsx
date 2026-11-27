import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import SearchableSelect from './SearchableSelect';

const labels = { label: 'Model', searchLabel: 'Search models', placeholder: 'Choose a model', emptyLabel: 'No matches', narrowLabel: 'Search for more' };

describe('searchable model selector', () => {
  it('navigates in displayed group order and restores focus after choosing or tabbing', () => {
    const onChange = vi.fn();
    render(<SearchableSelect {...labels} value="a" onChange={onChange} options={[
      { value: 'a', label: 'Alpha', group: 'Common' },
      { value: 'local', label: 'Local', group: 'Local' },
      { value: 'b', label: 'Beta', group: 'Common' },
    ]} />);
    const trigger = screen.getByRole('combobox', { name: 'Model' });
    fireEvent.click(trigger);
    expect(screen.getAllByRole('option').map((option) => option.value)).toEqual(['a', 'b', 'local']);
    const search = screen.getByRole('searchbox');
    expect(search).toHaveFocus();
    fireEvent.keyDown(search, { key: 'ArrowDown' });
    fireEvent.keyDown(search, { key: 'Enter' });
    expect(onChange).toHaveBeenCalledWith('b');
    expect(trigger).toHaveFocus();
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole('searchbox'), { key: 'Tab' });
    expect(trigger).toHaveFocus();
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('searches the full model list beyond the initial rendering limit and retains an unlisted ID', () => {
    const onChange = vi.fn();
    render(<SearchableSelect {...labels} value="saved-legacy-model" onChange={onChange}
      options={Array.from({ length: 250 }, (_, index) => ({ value: `model-${index}`, label: `Model ${index}` }))} />);
    expect(screen.getByRole('combobox')).toHaveTextContent('saved-legacy-model');
    fireEvent.click(screen.getByRole('combobox'));
    expect(screen.getAllByRole('option')).toHaveLength(200);
    expect(screen.getByText('Search for more')).toBeInTheDocument();
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'model-249' } });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    fireEvent.keyDown(screen.getByRole('searchbox'), { key: 'Enter' });
    expect(onChange).toHaveBeenCalledWith('model-249');
  });
});
