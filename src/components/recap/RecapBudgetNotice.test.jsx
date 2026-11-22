import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { createInstance } from 'i18next';
import { I18nextProvider } from 'react-i18next';
import zh from '../../i18n/locales/zh-CN.json';
import RecapBudgetNotice from './RecapBudgetNotice';

describe('recap budget notice', () => {
  it('shows the requested and available input with a non-blocking settings action', async () => {
    const i18n = createInstance();
    await i18n.init({ lng: 'zh-CN', resources: { 'zh-CN': { translation: zh } }, interpolation: { escapeValue: false } });
    const onAdjust = vi.fn();
    render(<I18nextProvider i18n={i18n}><RecapBudgetNotice
      error="RECAP_DAILY_INPUT_EXHAUSTED: limit=432000, used=424969, requested=10978, available=7031"
      target="recap.mainlines.title" onAdjust={onAdjust} /></I18nextProvider>);
    expect(screen.getByText('今日生成额度不足（10,978 tokens > 7,031 tokens），全天主线尚未更新。')).toBeVisible();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '调整额度' }));
    expect(onAdjust).toHaveBeenCalledOnce();
  });
});
