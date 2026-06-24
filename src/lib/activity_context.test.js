import { describe, expect, it } from 'vitest';
import {
  buildActivityContext,
  extractEntityFromTitle,
  extractEntityFromUrl,
  normalizeProcessLabel,
  siteNameFromUrl,
} from './activity_context';

describe('activity_context', () => {
  it('extracts site and repository entity from GitHub URLs', () => {
    expect(siteNameFromUrl('https://github.com/example/carbonPaper/pulls')).toEqual({
      host: 'github.com',
      siteName: 'GitHub',
    });
    expect(extractEntityFromUrl('https://github.com/example/carbonPaper/pulls')).toBe('carbonPaper');
  });

  it('cleans browser title suffixes and extracts repository names', () => {
    expect(extractEntityFromTitle('Pull requests · example/carbonPaper - GitHub', {
      siteName: 'GitHub',
      processName: 'chrome.exe',
    })).toBe('carbonPaper');
  });

  it('extracts project names from VS Code titles', () => {
    expect(normalizeProcessLabel('Code.exe')).toBe('Code');
    expect(extractEntityFromTitle('DetailCard.jsx - carbonPaper - Visual Studio Code', {
      processName: 'Code.exe',
    })).toBe('carbonPaper');
  });

  it('builds a compact activity label from current and related records', () => {
    const ctx = buildActivityContext({
      selectedEvent: {
        id: 10,
        appName: 'chrome.exe',
        windowTitle: 'Pull requests · example/carbonPaper - GitHub',
        timestamp: 1700000000000,
      },
      selectedRecord: {
        process_name: 'chrome.exe',
        window_title: 'Pull requests · example/carbonPaper - GitHub',
        page_url: 'https://github.com/example/carbonPaper/pulls',
      },
      relatedResult: {
        dominant_process: 'chrome.exe',
        snapshot_count: 12,
        screenshots: [],
      },
    });

    expect(ctx.title).toBe('GitHub - carbonPaper');
    expect(ctx.snapshotCount).toBe(12);
  });
});
