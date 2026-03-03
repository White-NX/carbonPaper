/**
 * Shared category constants used across Timeline, DetailCard, and AdvancedSearch.
 */

export const CATEGORY_LIST = [
  '编程开发', '学习教育', '影音娱乐', '社交通讯', '办公文档',
  '网页浏览', '游戏', '设计创作', '系统工具', '阅读资讯', '未分类',
];

export const CATEGORY_COLORS = {
  '编程开发': '#3b82f6',
  '学习教育': '#8b5cf6',
  '影音娱乐': '#ec4899',
  '社交通讯': '#10b981',
  '办公文档': '#f59e0b',
  '网页浏览': '#6366f1',
  '游戏': '#ef4444',
  '设计创作': '#14b8a6',
  '系统工具': '#6b7280',
  '阅读资讯': '#f97316',
};

/** Categories considered "entertainment" — hidden by default in task view. */
export const ENTERTAINMENT_CATEGORIES = new Set(['影音娱乐', '游戏']);

/** Categories considered "social" — hidden separately in task view. */
export const SOCIAL_CATEGORIES = new Set(['社交通讯']);
