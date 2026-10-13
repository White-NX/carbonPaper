import React from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import ErrorWindow from './ErrorWindow';
import { Dialog } from './Dialog';
import { OverlayCoordinatorProvider, useOverlayCoordinator } from './overlay';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

beforeEach(() => {
  vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue();
  vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

function Host({ fatal, onClose, onRestart = vi.fn(), onExit = vi.fn() }) {
  const coordinator = useOverlayCoordinator();
  return <OverlayCoordinatorProvider value={coordinator.value}>
    <header><button>Window control</button></header>
    <main>
      <Dialog isOpen title="Existing dialog" onClose={onClose}><button>Existing action</button></Dialog>
      <ErrorWindow isVisible={fatal} errors={['Synthetic failure']} onRestart={onRestart} onExit={onExit} />
    </main>
  </OverlayCoordinatorProvider>;
}

describe('fatal overlay', () => {
  it('takes keyboard ownership from an existing dialog and isolates the entire window', async () => {
    const onClose = vi.fn();
    const user = userEvent.setup();
    const view = render(<Host fatal={false} onClose={onClose} />);
    view.rerender(<Host fatal onClose={onClose} />);
    const fatal = screen.getByRole('alertdialog', { name: 'errorWindow.title' });
    expect(fatal).toHaveAttribute('aria-modal', 'true');
    expect(fatal.contains(document.activeElement)).toBe(true);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Window control' })).not.toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
    await user.tab({ shift: true });
    expect(within(fatal).getByRole('button', { name: 'errorWindow.switchToNormal' })).toHaveFocus();
    await user.tab();
    expect(within(fatal).getByRole('button', { name: 'errorWindow.restart' })).toHaveFocus();
    view.rerender(<Host fatal={false} onClose={onClose} />);
    expect(screen.getByRole('dialog', { name: 'Existing dialog' }).contains(document.activeElement)).toBe(true);
  });

  it('retains its role and actions in both presentation modes', async () => {
    const user = userEvent.setup();
    const onRestart = vi.fn();
    const onExit = vi.fn();
    render(<ErrorWindow isVisible errors={['Synthetic failure']} onRestart={onRestart} onExit={onExit} />);
    await user.click(screen.getByRole('button', { name: 'errorWindow.switchToNormal' }));
    const fatal = screen.getByRole('alertdialog', { name: 'errorWindow.title' });
    expect(fatal).toHaveTextContent('Synthetic failure');
    await user.click(within(fatal).getByRole('button', { name: 'errorWindow.restart' }));
    await user.click(within(fatal).getByRole('button', { name: 'errorWindow.exit' }));
    expect(onRestart).toHaveBeenCalledTimes(1);
    expect(onExit).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole('button', { name: 'errorWindow.switchToXibao' }));
    expect(screen.getByRole('alertdialog', { name: 'errorWindow.title' })).toBeVisible();
  });

  it('prevents a covered dialog from receiving Escape even without a coordinator', () => {
    const onClose = vi.fn();
    render(<><ErrorWindow isVisible errors={[]} /><Dialog isOpen title="Covered" onClose={onClose} /></>);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('alertdialog').contains(document.activeElement)).toBe(true);
  });
});
