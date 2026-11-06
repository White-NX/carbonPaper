import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { expect, it, vi } from 'vitest';
import AboutSection from './AboutSection';
import { RESTART_REQUIRED } from '../../lib/update_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: key => key }) }));

it('keeps an elevated client on the in-app update path after a normal relaunch', () => {
  const onDownloadUpdate = vi.fn();
  const onOpenDownloads = vi.fn();
  render(<AboutSection updateInfo={{ version: '1.2.3' }} updateError={RESTART_REQUIRED}
    onCheckUpdate={vi.fn()} onDownloadUpdate={onDownloadUpdate} onOpenDownloads={onOpenDownloads} />);
  expect(screen.getByText('updateModal.restartRequired')).toBeInTheDocument();
  expect(screen.queryByText('updateModal.installerRequired')).not.toBeInTheDocument();
  expect(screen.queryByText(RESTART_REQUIRED)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'updateModal.retry' }));
  expect(onDownloadUpdate).toHaveBeenCalledOnce();
  expect(onOpenDownloads).not.toHaveBeenCalled();
});
