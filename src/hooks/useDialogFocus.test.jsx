import React, { useState } from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { Dialog } from '../components/Dialog';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { OverlayShell } from '../components/overlay/OverlayShell';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

describe('dialog focus and isolation', () => {
  it('keeps the outer scroll lock, restores nested focus, and handles Escape only once', async () => {
    function Nested() {
      const [open, setOpen] = useState(false);
      const [confirm, setConfirm] = useState(false);
      return <>
        <button onClick={() => setOpen(true)}>Open</button>
        <Dialog isOpen={open} title="Editor" onClose={() => setOpen(false)}>
          <button onClick={() => setConfirm(true)}>Delete</button>
          <ConfirmDialog isOpen={confirm} title="Confirm deletion" onCancel={() => setConfirm(false)} />
        </Dialog>
      </>;
    }
    document.body.style.overflow = 'clip';
    const user = userEvent.setup();
    const view = render(<Nested />);
    const trigger = screen.getByRole('button', { name: 'Open' });
    await user.click(trigger);
    const editor = screen.getByRole('dialog', { name: 'Editor' });
    const deleteButton = within(editor).getByRole('button', { name: 'Delete' });
    await user.click(deleteButton);
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog', { name: 'Confirm deletion' })).toBeVisible();
    expect(editor.closest('[inert]')).not.toBeNull();
    expect(document.body.style.overflow).toBe('hidden');
    await user.keyboard('{Escape}');
    expect(screen.getByRole('dialog', { name: 'Editor' })).toBeVisible();
    expect(deleteButton).toHaveFocus();
    expect(document.body.style.overflow).toBe('hidden');
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(document.body.style.overflow).toBe('clip');
    view.unmount();
    document.body.style.overflow = '';
  });

  it('honors autofocus and excludes hidden or disabled controls from the Tab loop', async () => {
    const user = userEvent.setup();
    render(<Dialog isOpen title="Form" hideCloseButton onClose={vi.fn()}>
      <button hidden>Hidden</button><div style={{ display: 'none' }}><button>Hidden by CSS</button></div>
      <fieldset disabled><input aria-label="Disabled" /></fieldset>
      <input autoFocus aria-label="Name" /><button>Save</button>
    </Dialog>);
    expect(screen.getByRole('textbox', { name: 'Name' })).toHaveFocus();
    await user.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'Save' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('textbox', { name: 'Name' })).toHaveFocus();
  });

  it('does not restore focus outside the remaining top dialog when a lower dialog disappears', () => {
    const view = render(<>
      <Dialog isOpen title="Lower" onClose={vi.fn()}><button>Lower action</button></Dialog>
      <ConfirmDialog isOpen title="Upper" />
    </>);
    const upper = screen.getByRole('dialog', { name: 'Upper' });
    expect(upper.contains(document.activeElement)).toBe(true);
    view.rerender(<>
      <Dialog isOpen={false} title="Lower" onClose={vi.fn()} />
      <ConfirmDialog isOpen title="Upper" />
    </>);
    expect(upper.contains(document.activeElement)).toBe(true);
    expect(document.body.style.overflow).toBe('hidden');
  });

  it('keeps an allowed discard decision above a lock that mounts later', () => {
    const onCancel = vi.fn();
    const view = render(<><ConfirmDialog isOpen allowWhenLocked title="Discard" onCancel={onCancel} /><OverlayShell open={false} layer="gate" title="Lock" /></>);
    view.rerender(<><ConfirmDialog isOpen allowWhenLocked title="Discard" onCancel={onCancel} /><OverlayShell layer="gate" title="Lock"><input autoFocus aria-label="Lower focus" /><button>Unlock</button></OverlayShell></>);
    expect(screen.getByRole('dialog', { name: 'Discard' }).contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('isolates a startup content area while leaving its window controls available', () => {
    render(<div><button>Minimize</button><main><button>Private action</button><OverlayShell title="Setup"><button>Continue</button></OverlayShell></main></div>);
    expect(screen.getByRole('button', { name: 'Minimize' })).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Private action' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Continue' })).toHaveFocus();
  });

  it('restores pre-existing background attributes when the final dialog closes', () => {
    const view = render(<Dialog isOpen={false} title="Modal" onClose={vi.fn()} />);
    view.container.setAttribute('inert', '');
    view.container.setAttribute('aria-hidden', 'true');
    view.rerender(<Dialog isOpen title="Modal" onClose={vi.fn()} />);
    view.rerender(<Dialog isOpen={false} title="Modal" onClose={vi.fn()} />);
    expect(view.container).toHaveAttribute('inert');
    expect(view.container).toHaveAttribute('aria-hidden', 'true');
  });
});
