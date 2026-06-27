import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => {
      if (options && typeof options.count === 'number') {
        return `${key}:${options.count}`;
      }
      return key;
    },
  }),
}));

vi.mock('../lib/task_api', () => ({
  deleteTask: vi.fn(async () => undefined),
  getTaskScreenshots: vi.fn(async () => []),
  removeTaskScreenshot: vi.fn(async () => 1),
  updateTaskLabel: vi.fn(async () => undefined),
}));

vi.mock('../lib/monitor_api', () => ({
  fetchThumbnailBatch: vi.fn(async () => ({})),
}));

import ActivityContextDrawer from './ActivityContextDrawer';
import { deleteTask, getTaskScreenshots, removeTaskScreenshot } from '../lib/task_api';

const snapshot = {
  screenshot_id: 42,
  image_path: 'image.webp',
  process_name: 'Code.exe',
  window_title: 'carbonPaper',
  timestamp: 1_700_000_000,
  created_at: '2026-01-02 03:04:05',
  page_url: 'https://github.com/example/carbonPaper',
};

function renderDrawer(props = {}) {
  return render(
    <ActivityContextDrawer
      relatedResult={{
        task_id: 7,
        task_label: 'GitHub - carbonPaper',
        snapshot_count: 1,
      }}
      activityContext={{
        title: 'GitHub - carbonPaper',
        snapshotCount: 1,
      }}
      onClose={vi.fn()}
      onSelectScreenshot={vi.fn()}
      onActivityChanged={vi.fn()}
      {...props}
    />
  );
}

describe('ActivityContextDrawer', () => {
  beforeEach(() => {
    localStorage.clear();
    getTaskScreenshots.mockResolvedValue([snapshot]);
    removeTaskScreenshot.mockResolvedValue(1);
    deleteTask.mockResolvedValue(undefined);
  });

  it('asks for confirmation before removing a snapshot from an activity', async () => {
    renderDrawer();

    const unlinkButton = await screen.findByLabelText('activityContext.removeSnapshot');
    expect(unlinkButton.className).toContain('left-1');
    expect(unlinkButton.className).not.toContain('right-1');

    fireEvent.click(unlinkButton);

    expect(removeTaskScreenshot).not.toHaveBeenCalled();
    expect(screen.getByText('activityContext.removeSnapshotConfirm')).toBeInTheDocument();

    const confirmButton = screen
      .getAllByRole('button', { name: 'activityContext.removeSnapshot' })
      .find((button) => button.textContent === 'activityContext.removeSnapshot');
    fireEvent.click(confirmButton);

    await waitFor(() => {
      expect(removeTaskScreenshot).toHaveBeenCalledWith(7, 42);
    });
  });

  it('does not delete an activity when the confirmation is cancelled', async () => {
    renderDrawer();

    fireEvent.click(screen.getByRole('button', { name: 'activityContext.deleteActivity' }));
    expect(deleteTask).not.toHaveBeenCalled();
    expect(screen.getByText('activityContext.deleteActivityConfirm')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'common.cancel' }));

    expect(deleteTask).not.toHaveBeenCalled();
  });

  it('closes when the preview backdrop is clicked but not when the drawer itself is clicked', async () => {
    const onClose = vi.fn();
    const { getByTestId } = renderDrawer({ onClose });

    fireEvent.mouseDown(await screen.findByText('GitHub - carbonPaper'));

    expect(onClose).not.toHaveBeenCalled();

    fireEvent.mouseDown(getByTestId('activity-context-backdrop'));

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('keeps the drawer open when selecting a snapshot card', async () => {
    const onClose = vi.fn();
    const onSelectScreenshot = vi.fn();

    renderDrawer({ onClose, onSelectScreenshot });

    fireEvent.click(await screen.findByText('carbonPaper'));

    expect(onSelectScreenshot).toHaveBeenCalledWith(expect.objectContaining({
      screenshot_id: 42,
      id: 42,
      path: 'image.webp',
    }));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('uses the activity context card click preference for standalone preview', async () => {
    localStorage.setItem('cardClickBehavior_activityContext', 'standalone');
    const onSelectScreenshot = vi.fn();
    const onOpenFloatingPreview = vi.fn();

    renderDrawer({ onSelectScreenshot, onOpenFloatingPreview });

    fireEvent.click(await screen.findByText('carbonPaper'));

    expect(onOpenFloatingPreview).toHaveBeenCalledWith(expect.objectContaining({
      screenshot_id: 42,
      id: 42,
      path: 'image.webp',
    }));
    expect(onSelectScreenshot).not.toHaveBeenCalled();

    fireEvent.click(screen.getByLabelText('previewAction.openMainPreview'));

    expect(onSelectScreenshot).toHaveBeenCalledWith(expect.objectContaining({
      screenshot_id: 42,
      id: 42,
      path: 'image.webp',
    }));
  });
});
