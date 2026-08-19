import React from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, values) => (values && typeof values.label === 'string' ? `${key}:${values.label}` : key),
  }),
}));

import CaptureFiltersSection from './CaptureFiltersSection';

function renderSection(onQuickDelete = vi.fn()) {
  return render(
    <CaptureFiltersSection
      filterSettings={{ processes: [], titles: [], ignoreProtected: false }}
      processInput=""
      titleInput=""
      onProcessInputChange={vi.fn()}
      onTitleInputChange={vi.fn()}
      onAddProcess={vi.fn()}
      onAddTitle={vi.fn()}
      onRemoveProcess={vi.fn()}
      onRemoveTitle={vi.fn()}
      onToggleProtected={vi.fn()}
      onSave={vi.fn()}
      filtersDirty={false}
      savingFilters={false}
      saveFiltersMessage=""
      onQuickDelete={onQuickDelete}
      isDeleting={false}
      deleteMessage=""
    />
  );
}

describe('CaptureFiltersSection quick delete confirmation', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('cancels a quick delete without invoking the destructive action', async () => {
    const onQuickDelete = vi.fn();
    const user = userEvent.setup();
    renderSection(onQuickDelete);

    await user.click(screen.getAllByRole('button', { name: /settings\.captureFilters\.quickDelete\.button/ })[0]);
    expect(screen.getByRole('dialog')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'settings.captureFilters.quickDelete.no' }));

    expect(onQuickDelete).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('locks the quick delete confirmation while the action is running', async () => {
    let resolveDelete;
    const onQuickDelete = vi.fn(() => new Promise((resolve) => {
      resolveDelete = resolve;
    }));
    const user = userEvent.setup();
    renderSection(onQuickDelete);

    await user.click(screen.getAllByRole('button', { name: /settings\.captureFilters\.quickDelete\.button/ })[0]);
    await user.click(screen.getByRole('button', { name: 'settings.captureFilters.quickDelete.yes' }));

    expect(screen.getByRole('button', { name: 'common.processing' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'settings.captureFilters.quickDelete.no' })).toBeDisabled();
    expect(onQuickDelete).toHaveBeenCalledWith(5);

    resolveDelete();
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });
});
