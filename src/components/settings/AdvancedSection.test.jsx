import React from 'react';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import AdvancedSection from './AdvancedSection';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('./organize/useModelInventory', () => ({ useModelInventory: () => ({ models: [] }) }));
vi.mock('./advanced/DeveloperTools', () => ({ default: () => null }));

function domains(status = null) {
  return {
    preferences: {
      config: { cpu_limit_enabled: false, cpu_limit_percent: 10, use_dml: false, ocr_timeout_secs: 120 },
      gpus: [],
      handleToggle: vi.fn(),
    },
    ocr: { modelStatus: { installed: true }, refreshModel: vi.fn() },
    indexes: { status: null, refresh: vi.fn() },
    scheduler: { status, refresh: vi.fn() },
  };
}

describe('advanced settings domain props', () => {
  it('renders scheduler diagnostics and sends actions to their owning domains', async () => {
    const props = domains({
      execution_profile: 'balanced',
      blocked_reason: 'foreground_request',
      pauses: { total: 3, last_yield_ms: 12.6 },
    });
    render(<AdvancedSection {...props} />);
    await userEvent.click(screen.getByText('settings.advanced.background_processing.diagnostics'));
    expect(screen.getByText('balanced')).toBeVisible();
    expect(screen.getByText('foreground_request')).toBeVisible();
    expect(screen.getByText('13 ms')).toBeVisible();
    const diagnostics = screen.getByText('settings.advanced.background_processing.diagnostics').closest('details');
    await userEvent.click(within(diagnostics).getByRole('button', { name: 'common.refresh' }));
    expect(props.scheduler.refresh).toHaveBeenCalledOnce();
    expect(props.indexes.refresh).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('switch', { name: 'settings.advanced.cpu.label' }));
    expect(props.preferences.handleToggle).toHaveBeenCalledWith('cpu_limit_enabled');
    await userEvent.click(screen.getByRole('button', { name: 'settings.advanced.ocrHealth.refresh' }));
    expect(props.ocr.refreshModel).toHaveBeenCalledOnce();
  });

  it('renders diagnostics while scheduler status is still loading', async () => {
    render(<AdvancedSection {...domains()} />);
    await userEvent.click(screen.getByText('settings.advanced.background_processing.diagnostics'));
    expect(screen.getByText('settings.advanced.background_processing.yieldLatency').nextElementSibling).toHaveTextContent('—');
  });
});
