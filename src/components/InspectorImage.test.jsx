import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import InspectorImage from './InspectorImage';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key }),
}));

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    disconnect() {}
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function renderPreview() {
  const { container } = render(<InspectorImage item={{ imageUrl: 'preview.png' }} />);
  const preview = container.firstChild;
  vi.spyOn(preview, 'getBoundingClientRect').mockReturnValue({
    width: 800, height: 600, left: 0, top: 0, right: 800, bottom: 600,
  });
  return { preview, image: screen.getByRole('img').parentElement };
}

describe('preview zoom controls', () => {
  it.each([
    ['zoomIn', 2.25, -500, -375],
    ['zoomOut', 1 / 2.25, 400 * (1 - 1 / 2.25), 300 * (1 - 1 / 2.25)],
  ])('keeps consecutive %s clicks from resetting the image', async (action, scale, x, y) => {
    const user = userEvent.setup();
    const { image } = renderPreview();

    await user.dblClick(screen.getByRole('button', { name: `snapshotPreview.${action}` }));

    const [, actualX, actualY, actualScale] = image.style.transform.match(/translate\(([-\d.]+)px, ([-\d.]+)px\) scale\(([-\d.]+)\)/);
    expect(Number(actualScale)).toBeCloseTo(scale);
    expect(Number(actualX)).toBeCloseTo(x);
    expect(Number(actualY)).toBeCloseTo(y);
    expect(screen.getByText(`${Math.round(scale * 100)}%`)).toBeInTheDocument();
  });

  it('chooses a preset without resetting the current scale when opening the menu', async () => {
    const user = userEvent.setup();
    const { image } = renderPreview();
    await user.click(screen.getByRole('button', { name: 'snapshotPreview.zoomIn' }));

    const selector = screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 150%' });
    await user.click(selector);
    expect(image).toHaveStyle({ transform: 'translate(-200px, -150px) scale(1.5)' });
    expect(screen.getByRole('menuitemradio', { name: '150%' })).toHaveAttribute('aria-checked', 'true');

    await user.click(screen.getByRole('menuitemradio', { name: '200%' }));
    expect(image).toHaveStyle({ transform: 'translate(-400px, -300px) scale(2)' });
    expect(selector).toHaveTextContent('200%');
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(selector).toHaveFocus();

    await user.dblClick(screen.getByRole('button', { name: 'snapshotPreview.zoomIn' }));
    expect(selector).toHaveTextContent('450%');
  });

  it('supports keyboard preset selection and dismisses the menu on Escape or an outside press', async () => {
    const user = userEvent.setup();
    const { preview, image } = renderPreview();
    const selector = screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 100%' });
    selector.focus();
    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('menuitemradio', { name: '100%' })).toHaveFocus();
    await user.keyboard('{ArrowDown}{Enter}');
    expect(selector).toHaveTextContent('125%');
    expect(image).toHaveStyle({ transform: 'translate(-100px, -75px) scale(1.25)' });

    await user.click(selector);
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(selector).toHaveFocus();
    await user.click(selector);
    fireEvent.pointerDown(preview, { button: 0 });
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(selector).toHaveTextContent('125%');
  });

  it('keeps menu wheel gestures separate from image zoom and retains both fit actions', async () => {
    const user = userEvent.setup();
    const { preview, image } = renderPreview();
    const selector = screen.getByRole('button', { name: 'snapshotPreview.chooseZoom: 100%' });
    await user.click(selector);
    fireEvent.wheel(screen.getByRole('menu'), { deltaY: -100, clientX: 400, clientY: 300 });
    expect(selector).toHaveTextContent('100%');
    await user.click(screen.getByRole('menuitemradio', { name: '200%' }));
    await user.click(screen.getByRole('button', { name: 'snapshotPreview.fitToWindow' }));
    expect(image).toHaveStyle({ transform: 'translate(0px, 0px) scale(1)' });

    fireEvent.wheel(preview, { deltaY: -100, clientX: 400, clientY: 300 });
    expect(selector).toHaveTextContent('115%');
    await user.dblClick(image);
    expect(image).toHaveStyle({ transform: 'translate(0px, 0px) scale(1)' });
  });
});
