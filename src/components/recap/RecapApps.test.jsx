import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import RecapApps from './RecapApps';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

describe('recap app icons', () => {
  it('expands and collapses apps without removing their accessible names', () => {
    const apps = Array.from({ length: 8 }, (_, i) => ({ name: `App ${i + 1}`, icon: null }));
    render(<RecapApps apps={apps} />);
    expect(screen.getAllByRole('img')).toHaveLength(6);
    const more = screen.getByRole('button', { name: 'recap.moreApps' });
    expect(more).toHaveTextContent('+2');
    fireEvent.click(more);
    expect(screen.getByRole('img', { name: 'App 8' })).toHaveAttribute('title', 'App 8');
    expect(screen.getAllByRole('img')).toHaveLength(8);
    const less = screen.getByRole('button', { name: 'recap.fewerApps' });
    expect(less).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(less);
    expect(screen.queryByRole('img', { name: 'App 8' })).not.toBeInTheDocument();
  });

  it('falls back for missing, remote and broken icons and recovers when the icon changes', () => {
    const apps = [{ name: 'Missing' }, { name: 'Remote', icon: 'https://example.com/icon.png' }, { name: 'Editor', icon: 'aWNvbg==' }];
    const { container, rerender } = render(<RecapApps apps={apps} />);
    expect(container.querySelectorAll('img')).toHaveLength(1);
    const icon = container.querySelector('img');
    expect(icon).toHaveAttribute('src', 'data:image/png;base64,aWNvbg==');
    fireEvent.error(icon);
    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByRole('img', { name: 'Editor' })).toBeVisible();
    rerender(<RecapApps apps={[{ name: 'Editor', icon: 'data:image/png;base64,bmV3' }]} />);
    expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/png;base64,bmV3');
  });
});
