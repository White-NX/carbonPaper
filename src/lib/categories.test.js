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

  it('keeps display colors aligned with selectable categories', () => {
    const categoriesWithDedicatedColors = CATEGORY_LIST.filter((category) => category !== '未分类');

    expect(Object.keys(CATEGORY_COLORS).sort()).toEqual([...categoriesWithDedicatedColors].sort());
    expect(CATEGORY_COLORS['未分类']).toBeUndefined();

    for (const color of Object.values(CATEGORY_COLORS)) {
      expect(color).toMatch(/^#[0-9a-f]{6}$/i);
    }
  });

  it('keeps task filter groups known and non-overlapping', () => {
    const knownCategories = new Set(CATEGORY_LIST);

    for (const category of ENTERTAINMENT_CATEGORIES) {
      expect(knownCategories.has(category)).toBe(true);
      expect(SOCIAL_CATEGORIES.has(category)).toBe(false);
    }

    for (const category of SOCIAL_CATEGORIES) {
      expect(knownCategories.has(category)).toBe(true);
      expect(ENTERTAINMENT_CATEGORIES.has(category)).toBe(false);
    }
  });
});
