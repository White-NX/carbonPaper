import { describe, expect, it } from 'vitest';
import {
  CATEGORY_COLORS,
  CATEGORY_LIST,
  ENTERTAINMENT_CATEGORIES,
  SOCIAL_CATEGORIES,
} from './categories';

describe('categories constants', () => {
  it('contains expected baseline categories', () => {
    expect(CATEGORY_LIST).toContain('编程开发');
    expect(CATEGORY_LIST).toContain('社交通讯');
    expect(CATEGORY_LIST).toContain('未分类');
  });

  it('provides colors for primary categories', () => {
    expect(CATEGORY_COLORS['编程开发']).toBe('#3b82f6');
    expect(CATEGORY_COLORS['办公文档']).toBe('#f59e0b');
    expect(CATEGORY_COLORS['阅读资讯']).toBe('#f97316');
  });

  it('marks entertainment and social groups', () => {
    expect(ENTERTAINMENT_CATEGORIES.has('影音娱乐')).toBe(true);
    expect(ENTERTAINMENT_CATEGORIES.has('游戏')).toBe(true);
    expect(ENTERTAINMENT_CATEGORIES.has('编程开发')).toBe(false);

    expect(SOCIAL_CATEGORIES.has('社交通讯')).toBe(true);
    expect(SOCIAL_CATEGORIES.has('办公文档')).toBe(false);
  });
});
