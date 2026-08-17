import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => (options?.name ? `${key}:${options.name}` : key),
  }),
}));

vi.mock('../lib/monitor_api', () => ({
  resumeOfficeDocument: vi.fn(async () => ({ status: 'opened' })),
}));

import { resumeOfficeDocument } from '../lib/monitor_api';
import OfficeDocumentEntry from './OfficeDocumentEntry';

describe('OfficeDocumentEntry', () => {
  beforeEach(() => {
    resumeOfficeDocument.mockClear();
  });

  it('shows the application and opens a resumable document', async () => {
    render(
      <OfficeDocumentEntry
        screenshotId={42}
        documentRef={{
          application: 'excel',
          kind: 'cloud_document',
          display_name: 'Budget.xlsx',
          resumable: true,
        }}
      />,
    );

    const openButton = screen.getByRole('button');
    expect(openButton).toHaveTextContent('documentSource.applications.excel');
    expect(openButton).toHaveTextContent('documentSource.kinds.cloud');
    fireEvent.click(openButton);

    await waitFor(() => {
      expect(resumeOfficeDocument).toHaveBeenCalledWith(42);
    });
  });

  it('keeps an unsaved document visible but disabled', () => {
    render(
      <OfficeDocumentEntry
        screenshotId={42}
        documentRef={{
          application: 'word',
          kind: 'unsaved',
          display_name: 'Document1',
          resumable: false,
        }}
      />,
    );

    expect(screen.getByText('Document1')).toBeInTheDocument();
    expect(screen.getByRole('button')).toBeDisabled();
  });
});
