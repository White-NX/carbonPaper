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
import SnapshotSourceAction from './SnapshotSourceAction';

const documentRef = {
  application: 'word',
  kind: 'local_file',
  display_name: 'Report.docx',
  resumable: true,
};

describe('SnapshotSourceAction', () => {
  beforeEach(() => {
    resumeOfficeDocument.mockClear();
  });

  it('opens the associated Office document directly when it is the only source', async () => {
    render(<SnapshotSourceAction documentRef={documentRef} screenshotId={42} />);

    fireEvent.click(screen.getByRole('button', { name: 'snapshotSource.open' }));

    await waitFor(() => {
      expect(resumeOfficeDocument).toHaveBeenCalledWith(42);
    });
  });

  it('asks the user to choose when a document and page are both available', async () => {
    const onOpenUrl = vi.fn(async () => undefined);
    render(
      <SnapshotSourceAction
        documentRef={documentRef}
        screenshotId={42}
        pageUrl="https://example.com/work"
        onOpenUrl={onOpenUrl}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'snapshotSource.open' }));
    fireEvent.click(screen.getByRole('menuitem', { name: /Report\.docx/ }));

    await waitFor(() => {
      expect(resumeOfficeDocument).toHaveBeenCalledWith(42);
      expect(onOpenUrl).not.toHaveBeenCalled();
    });
  });

  it('opens a page source when no document is available', async () => {
    const onOpenUrl = vi.fn(async () => undefined);
    render(
      <SnapshotSourceAction
        screenshotId={42}
        pageUrl="https://example.com/work"
        onOpenUrl={onOpenUrl}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'snapshotSource.open' }));

    await waitFor(() => {
      expect(onOpenUrl).toHaveBeenCalledWith('https://example.com/work');
    });
  });
});
