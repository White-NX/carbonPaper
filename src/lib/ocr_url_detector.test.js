import { describe, expect, it } from 'vitest';
import { extractUrlsFromOcr, hasUrlsInOcr } from './ocr_url_detector';

describe('ocr_url_detector', () => {
  it('extracts and normalizes urls from OCR results', () => {
    const ocrResults = [
      { text: 'Visit https://example.com and www.test.com for details' },
      { text: 'Duplicate link https://example.com' },
    ];

    const urls = extractUrlsFromOcr(ocrResults);

    expect(urls).toEqual(['https://example.com', 'https://www.test.com']);
  });

  it('returns empty list for invalid input', () => {
    expect(extractUrlsFromOcr(null)).toEqual([]);
    expect(extractUrlsFromOcr('bad input')).toEqual([]);
  });

  it('detects url existence correctly', () => {
    expect(hasUrlsInOcr([{ text: 'See www.example.org now' }])).toBe(true);
    expect(hasUrlsInOcr([{ text: 'No links here' }])).toBe(false);
  });
});
