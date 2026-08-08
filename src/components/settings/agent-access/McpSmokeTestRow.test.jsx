import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import McpSmokeTestRow from './McpSmokeTestRow';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, values = {}) => {
      if (key.endsWith('.success')) return `passed v${values.version} ${values.count}`;
      if (key.includes('.errors.')) return key.split('.').at(-1);
      if (key.includes('.stages.')) return key.split('.').at(-1);
      if (key.endsWith('.duration')) return `${values.duration} ms`;
      return key;
    },
  }),
}));

describe('McpSmokeTestRow', () => {
  it('renders every stage and reruns the probe', () => {
    const onRun = vi.fn();
    render(
      <McpSmokeTestRow
        loading={false}
        onRun={onRun}
        report={{
          ok: true,
          tool_schema_version: 2,
          advertised_tool_count: 12,
          stages: [
            { id: 'initialize', status: 'passed', duration_ms: 1 },
            { id: 'ping', status: 'passed', duration_ms: 2 },
            { id: 'tools_list', status: 'passed', duration_ms: 3 },
            { id: 'metadata_query', status: 'passed', duration_ms: 4 },
          ],
        }}
      />,
    );

    expect(screen.getByText('passed v2 12')).toBeInTheDocument();
    expect(screen.getByText('metadata_query')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button'));
    expect(onRun).toHaveBeenCalledTimes(1);
  });

  it('shows the stable failure code through localization', () => {
    render(
      <McpSmokeTestRow
        loading={false}
        onRun={vi.fn()}
        report={{
          ok: false,
          failure_kind: 'privacy_filter_error',
          stages: [],
        }}
      />,
    );

    expect(screen.getByText('privacy_filter_error')).toBeInTheDocument();
  });

  it('does not start a test while another MCP operation is active', () => {
    const onRun = vi.fn();
    render(
      <McpSmokeTestRow
        loading={false}
        disabled
        onRun={onRun}
        report={null}
      />,
    );

    const button = screen.getByRole('button');
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(onRun).not.toHaveBeenCalled();
  });
});
