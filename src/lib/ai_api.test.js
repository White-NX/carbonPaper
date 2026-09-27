import { describe, expect, it } from 'vitest';
import { aiErrorDetail, aiErrorKey } from './ai_api';

describe('ai error mapping', () => {
  it('maps backend codes to message keys and keeps the detail', () => {
    expect(aiErrorKey('AI_NOT_FOUND: Model not found')).toBe('ai.errors.AI_NOT_FOUND');
    expect(aiErrorDetail('AI_NOT_FOUND: Model not found')).toBe('Model not found');
    expect(aiErrorKey(new Error('AUTH_REQUIRED'))).toBe('ai.errors.AUTH_REQUIRED');
    expect(aiErrorKey('something else')).toBe('ai.errors.unknown');
    expect(aiErrorDetail('AI_TIMEOUT')).toBe('');
  });
});
