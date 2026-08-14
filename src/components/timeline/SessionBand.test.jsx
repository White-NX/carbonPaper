import React from 'react';
import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key }),
}));

import SessionBand from './SessionBand';

/** Milliseconds map to pixels one for one, so widths read straight off the block. */
const timeToX = (time) => time;

const BLOCK = {
  id: 'block-1',
  kind: 'app',
  start: 0,
  end: 400,
  appName: 'msedge.exe',
  processIcon: 'iVBORw0KGgo=',
  windowCount: 3,
  interruptions: 0,
  segments: [{ key: 's1', start: 0, appName: 'msedge.exe', windowTitle: 'Example' }],
};

function renderBand() {
  const { container } = render(
    <SessionBand blocks={[BLOCK]} timeToX={timeToX} width={500} height={28} />,
  );
  return container;
}

/**
 * The camera moves a band by scaling it horizontally, which is right for a block
 * standing over a stretch of time and wrong for everything drawn inside one. The
 * correction is a class rather than a prop, so nothing fails loudly when a label
 * loses it — hence these.
 */
describe('SessionBand under a scaled camera', () => {
  it('takes the horizontal scale back out of the icon', () => {
    const icon = renderBand().querySelector('img');
    expect(icon.className).toContain('tl-steady');
  });

  it('holds the app name to the left edge it is laid out against', () => {
    const name = renderBand().querySelector('.tl-steady-left');
    expect(name.className).toContain('tl-steady');
    expect(renderBand().textContent).toContain('msedge.exe');
  });

  it('holds the duration to the right edge `ml-auto` puts it on', () => {
    const duration = [...renderBand().querySelectorAll('span')]
      .find((node) => node.textContent === 'timeline.duration.seconds');
    expect(duration.className).toContain('tl-steady');
    expect(duration.className).toContain('tl-steady-right');
  });

  it('leaves the detail text clipped by a box the camera still stretches', () => {
    // The innermost match, because the detail is wrapped and both carry the text.
    const detail = [...renderBand().querySelectorAll('span')]
      .find((node) => node.textContent === 'timeline.session.windows' && node.children.length === 0);

    // The text itself is corrected...
    expect(detail.className).toContain('tl-steady');

    // ...but what cuts it off is not, so the slot it is cut to is the one the
    // block actually has on screen. Corrected, a band drawn wider than it is
    // now would let the detail run out over the duration beside it.
    expect(detail.parentElement.className).toContain('overflow-hidden');
    expect(detail.parentElement.className).not.toContain('tl-steady');
  });
});
