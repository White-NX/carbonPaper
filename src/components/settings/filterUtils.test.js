import { describe, expect, it } from 'vitest';
import { formatInvokeError, normalizeList } from './filterUtils';

describe('filterUtils', () => {
  it('normalizes list by commas/newlines and lowercases values', () => {
    const input = 'Chrome.EXE,  CODE.EXE\nNotepad.exe\n\n';

    expect(normalizeList(input)).toEqual(['chrome.exe', 'code.exe', 'notepad.exe']);
  });

  it('returns message for string and object errors', () => {
    expect(formatInvokeError('failed')).toBe('failed');
    expect(formatInvokeError({ message: 'boom' })).toBe('boom');
    expect(formatInvokeError({ code: 500 })).toBe('{"code":500}');
  });

  it('handles circular object and empty error as unknown', () => {
    const circular = {};
    circular.self = circular;

    expect(formatInvokeError(circular)).toBe('未知错误');
    expect(formatInvokeError(null)).toBe('未知错误');
  });
});
