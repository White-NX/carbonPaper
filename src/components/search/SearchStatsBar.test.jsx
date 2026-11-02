import React from 'react';
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { createInstance } from 'i18next';
import { I18nextProvider } from 'react-i18next';
import zh from '../../i18n/locales/zh-CN.json';
import en from '../../i18n/locales/en.json';
import { SearchStatsBar } from './SearchStatsBar';

async function renderStats(props, language = 'zh-CN') {
  const i18n = createInstance();
  await i18n.init({ lng: language, resources: { 'zh-CN': { translation: zh }, en: { translation: en } }, interpolation: { escapeValue: false } });
  return render(<I18nextProvider i18n={i18n}><SearchStatsBar {...props} /></I18nextProvider>);
}

describe('search result summary', () => {
  it('converts milliseconds to seconds with two decimal places', async () => {
    await renderStats({ count: 3, totalKnown: true, elapsedMs: 12268 });
    expect(screen.getByRole('status')).toHaveTextContent('共 3 个结果，用时 12.27 秒');
  });

  it('preserves hundredths and distinguishes loaded counts from known totals', async () => {
    await renderStats({ count: 50, totalKnown: false, elapsedMs: 1000 }, 'en');
    expect(screen.getByRole('status')).toHaveTextContent('50 results loaded, took 1.00 seconds');
  });

  it('omits unavailable timing and keeps retrieval status accessible', async () => {
    await renderStats({ count: 3, totalKnown: true, elapsedMs: NaN, statusMessage: '正在补充相关回顾…', updating: true });
    expect(screen.getByRole('status')).toHaveTextContent('共 3 个结果');
    expect(screen.getByRole('status')).not.toHaveTextContent('用时');
    expect(screen.getByRole('status')).toHaveAttribute('title', '正在补充相关回顾…');
  });
});
