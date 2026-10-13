import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import MigrationProgressDialog from './MigrationProgressDialog';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key, values) => values?.error ? `${key}: ${values.error}` : key }) }));

describe('storage migration dialog', () => {
  it('allows explicit cancellation while blocking Escape, the backdrop, and the close button', () => {
    const onCancel = vi.fn();
    render(<MigrationProgressDialog isOpen onCancel={onCancel} progress={{ copied_files: 2, total_files: 4 }} />);
    expect(screen.queryByRole('button', { name: 'common.close' })).not.toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '50');
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(screen.getByRole('dialog').parentElement);
    expect(onCancel).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'common.cancel' }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('waits for cancellation and shows a retryable error without hiding progress', () => {
    const onCancel = vi.fn();
    const view = render(<MigrationProgressDialog isOpen onCancel={onCancel} cancellation={{ pending: true }} />);
    expect(screen.getByRole('button')).toBeDisabled();
    view.rerender(<MigrationProgressDialog isOpen onCancel={onCancel} cancellation={{ requested: true }} />);
    expect(screen.getByRole('button')).toBeDisabled();
    view.rerender(<MigrationProgressDialog isOpen onCancel={onCancel} cancellation={{ error: 'AUTH_CANCELLED' }} />);
    expect(screen.getByRole('alert')).toHaveTextContent('AUTH_CANCELLED');
    expect(screen.getByRole('progressbar')).toBeVisible();
    expect(screen.getByRole('button', { name: 'common.cancel' })).toBeEnabled();
  });
});
