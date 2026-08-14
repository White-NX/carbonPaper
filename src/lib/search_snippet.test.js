import { describe, expect, it } from 'vitest';
import { buildSnippet, normalizeOcrText } from './search_snippet';

describe('normalizeOcrText', () => {
  it('joins CJK lines without inserting spaces', () => {
    expect(normalizeOcrText('上海地铁\n换乘名场面')).toBe('上海地铁换乘名场面');
  });

  it('keeps a space between latin fragments', () => {
    expect(normalizeOcrText('hello\nworld')).toBe('hello world');
  });

  it('separates CJK from latin with a space', () => {
    expect(normalizeOcrText('测试\nPASSED')).toBe('测试 PASSED');
  });

  it('collapses blank lines and repeated spaces', () => {
    expect(normalizeOcrText('  foo   bar \n\n\n  baz  ')).toBe('foo bar baz');
  });

  it('returns an empty string for missing input', () => {
    expect(normalizeOcrText('')).toBe('');
    expect(normalizeOcrText(null)).toBe('');
  });
});

describe('buildSnippet', () => {
  it('centers the window on the first matching token', () => {
    const text = `${'前'.repeat(120)}关键词${'后'.repeat(120)}`;
    const snippet = buildSnippet(text, ['关键词'], { radius: 10, maxLength: 40 });

    expect(snippet).toContain('关键词');
    expect(snippet.startsWith('…')).toBe(true);
    expect(snippet.endsWith('…')).toBe(true);
  });

  it('does not prefix an ellipsis when the hit is at the very beginning', () => {
    const snippet = buildSnippet(`关键词${'后'.repeat(200)}`, ['关键词'], { radius: 10, maxLength: 40 });
    expect(snippet.startsWith('…')).toBe(false);
    expect(snippet.endsWith('…')).toBe(true);
  });

  it('falls back to the head of the text when nothing matches', () => {
    const snippet = buildSnippet('abcdefghij', ['zzz'], { maxLength: 5 });
    expect(snippet).toBe('abcde…');
  });

  it('returns short text untouched', () => {
    expect(buildSnippet('短文本', [])).toBe('短文本');
  });

  it('matches tokens case-insensitively', () => {
    const snippet = buildSnippet('some ERROR happened here', ['error']);
    expect(snippet).toContain('ERROR');
  });

  it('returns an empty string when there is no OCR text', () => {
    expect(buildSnippet('', ['x'])).toBe('');
    expect(buildSnippet(null, ['x'])).toBe('');
  });
});
