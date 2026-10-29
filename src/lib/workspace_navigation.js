export const WORKSPACE_PAGES = [
  { id: 'preview', labelKey: 'activityBar.records' },
  { id: 'advanced-search', labelKey: 'activityBar.search' },
  { id: 'recap', labelKey: 'activityBar.recap' },
  { id: 'smart-cluster', labelKey: 'activityBar.topics' },
];

export function workspaceLabelKey(page) {
  return WORKSPACE_PAGES.find(({ id }) => id === page)?.labelKey || 'activityBar.records';
}
