import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { Dialog } from '../Dialog';
import { ConfirmDialog } from '../ConfirmDialog';
import { CorrectionDialog, SourcesDialog } from '../recap/RecapDialogs';
import { DialogVisibilityContext } from '../../hooks/useDialogFocus';
import { OverlayCoordinatorProvider, useOverlayCoordinator, useOverlaySlot } from './coordinator';
import { OverlayShell } from './OverlayShell';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

function Slot({ id }) {
  const visible = useOverlaySlot(id || 'auth', Boolean(id));
  return <OverlayShell open={visible} title="Startup surface"><button>Continue</button></OverlayShell>;
}

function Host({ authenticated = true, slot = null, children }) {
  const coordinator = useOverlayCoordinator();
  return <OverlayCoordinatorProvider value={coordinator.value}>
    <DialogVisibilityContext.Provider value={authenticated}>
      <Slot id={slot} />
      {children}
    </DialogVisibilityContext.Provider>
  </OverlayCoordinatorProvider>;
}

describe('ordinary dialog visibility', () => {
  it('hides an already open dialog immediately on lock without cancelling its owner', () => {
    const onClose = vi.fn();
    const content = <Dialog isOpen title="Private dialog" onClose={onClose}>Private content</Dialog>;
    const view = render(<Host>{content}</Host>);
    expect(screen.getByRole('dialog', { name: 'Private dialog' })).toBeVisible();
    view.rerender(<Host authenticated={false}>{content}</Host>);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    view.rerender(<Host>{content}</Host>);
    expect(screen.getByRole('dialog', { name: 'Private dialog' })).toBeVisible();
  });

  it.each(['auth', 'maintenance', 'fatal', 'update'])('yields ordinary confirmations to %s and restores them afterwards', (slot) => {
    const onCancel = vi.fn();
    const content = <ConfirmDialog isOpen title="Delete record" onCancel={onCancel} />;
    const view = render(<Host>{content}</Host>);
    view.rerender(<Host slot={slot}>{content}</Host>);
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog', { name: 'Startup surface' })).toBeVisible();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onCancel).not.toHaveBeenCalled();
    view.rerender(<Host>{content}</Host>);
    expect(screen.getByRole('dialog', { name: 'Delete record' })).toBeVisible();
  });

  it('permits the explicit locked-window discard action only over the auth gate', () => {
    const content = <ConfirmDialog isOpen allowWhenLocked title="Discard changes" />;
    const view = render(<Host authenticated={false} slot="auth">{content}</Host>);
    expect(screen.getByRole('dialog', { name: 'Discard changes' })).toBeVisible();
    view.rerender(<Host authenticated={false} slot="maintenance">{content}</Host>);
    expect(screen.queryByRole('dialog', { name: 'Discard changes' })).not.toBeInTheDocument();
    view.rerender(<Host authenticated={false} slot="fatal">{content}</Host>);
    expect(screen.queryByRole('dialog', { name: 'Discard changes' })).not.toBeInTheDocument();
  });

  it('preserves a recap correction draft while a startup prompt holds the screen', () => {
    const content = <CorrectionDialog date="2026-10-12" editing={{ kind: 'rename', task_id: 'task', task_title: 'Original' }}
      threads={[]} onSaved={vi.fn()} onClose={vi.fn()} />;
    const view = render(<Host>{content}</Host>);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'My draft' } });
    view.rerender(<Host slot="update">{content}</Host>);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    view.rerender(<Host>{content}</Host>);
    expect(screen.getByRole('textbox')).toHaveValue('My draft');
  });

  it('hides recap sources while a maintenance gate holds the screen', () => {
    const content = <SourcesDialog sources={[]} action={{}} time={vi.fn()} onClose={vi.fn()} />;
    const view = render(<Host>{content}</Host>);
    expect(screen.getByRole('dialog', { name: 'recap.sources' })).toBeVisible();
    view.rerender(<Host slot="maintenance">{content}</Host>);
    expect(screen.queryByRole('dialog', { name: 'recap.sources' })).not.toBeInTheDocument();
  });
});
