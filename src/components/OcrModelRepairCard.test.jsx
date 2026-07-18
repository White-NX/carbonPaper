import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key) => key,
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import OcrModelRepairCard from './OcrModelRepairCard';

describe('OcrModelRepairCard', () => {
  beforeEach(() => {
    invoke.mockImplementation((command) => {
      if (command === 'get_rust_ocr_model_status') {
        return Promise.resolve({ installed: false, path: 'C:\\models\\ocr' });
      }
      if (command === 'download_rust_ocr_model') {
        return Promise.resolve({ installed: true, path: 'C:\\models\\ocr' });
      }
      return Promise.reject(new Error(`Unexpected command: ${command}`));
    });
  });

  it('repairs the model directly without an authentication command', async () => {
    render(<OcrModelRepairCard isOpen onClose={vi.fn()} />);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_rust_ocr_model_status');
      expect(screen.getByRole('button', { name: 'ocrModelRepair.repair' })).toBeEnabled();
    });

    fireEvent.click(screen.getByRole('button', { name: 'ocrModelRepair.repair' }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('download_rust_ocr_model');
      expect(screen.getByText('ocrModelRepair.ready')).toBeInTheDocument();
    });

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      'get_rust_ocr_model_status',
      'download_rust_ocr_model',
    ]);
  });
});
