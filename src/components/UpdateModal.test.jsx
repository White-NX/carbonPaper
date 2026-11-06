import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { expect, it, vi } from 'vitest';
import { UpdateModal } from './UpdateModal';
import { ELEVATION_CANCELLED, INSTALLER_REQUIRED, RESTART_REQUIRED } from '../lib/update_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: key => key }) }));

it('offers the installer when this installation needs repair before automatic updates', () => {
  const onDownload = vi.fn();
  const onOpenDownloads = vi.fn();
  render(<UpdateModal isVisible updateInfo={{ version: '1.2.3' }}
    downloadError={INSTALLER_REQUIRED} onDownload={onDownload}
    onOpenDownloads={onOpenDownloads} onLater={vi.fn()} onClose={vi.fn()} />);
  expect(screen.getByText('updateModal.installerRequired')).toBeInTheDocument();
  expect(screen.queryByText(INSTALLER_REQUIRED)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'updateModal.openDownloads' }));
  expect(onOpenDownloads).toHaveBeenCalledOnce();
  expect(onDownload).not.toHaveBeenCalled();
});

it('keeps retry available after the Windows permission request is cancelled', () => {
  const onDownload = vi.fn();
  const onOpenDownloads = vi.fn();
  render(<UpdateModal isVisible updateInfo={{ version: '1.2.3' }}
    downloadError={ELEVATION_CANCELLED} onDownload={onDownload}
    onOpenDownloads={onOpenDownloads} onLater={vi.fn()} onClose={vi.fn()} />);
  expect(screen.getByText('updateModal.elevationCancelled')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'updateModal.retry' }));
  expect(onDownload).toHaveBeenCalledOnce();
  expect(onOpenDownloads).not.toHaveBeenCalled();
});

it('guides an elevated legacy client to relaunch normally and retry the in-app update', () => {
  const onDownload = vi.fn();
  const onOpenDownloads = vi.fn();
  render(<UpdateModal isVisible updateInfo={{ version: '1.2.3' }}
    downloadError={RESTART_REQUIRED} onDownload={onDownload}
    onOpenDownloads={onOpenDownloads} onLater={vi.fn()} onClose={vi.fn()} />);
  expect(screen.getByText('updateModal.restartRequired')).toBeInTheDocument();
  expect(screen.queryByText('updateModal.installerRequired')).not.toBeInTheDocument();
  expect(screen.queryByText(RESTART_REQUIRED)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'updateModal.retry' }));
  expect(onDownload).toHaveBeenCalledOnce();
  expect(onOpenDownloads).not.toHaveBeenCalled();
});
