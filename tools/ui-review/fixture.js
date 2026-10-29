export const REVIEW_DATE = '2026-10-28';
export const REVIEW_START = Date.parse(`${REVIEW_DATE}T02:00:00Z`);

export const snapshots = Array.from({ length: 24 }, (_, index) => ({
  id: index + 1, screenshot_id: index + 1, image_path: `memory://review-${index + 1}`,
  timestamp: (REVIEW_START + index * 60000) / 1000, captured_at_ms: REVIEW_START + index * 60000,
  created_at: new Date(REVIEW_START + index * 60000).toISOString(),
  process_name: index % 3 === 0 ? 'Browser' : index % 3 === 1 ? 'Editor' : 'Notes',
  window_title: index % 3 === 0 ? 'Project reference — CarbonPaper UI review' : index % 3 === 1 ? 'Workspace navigation — design notes' : 'Review checklist — screenshot sources',
  text: `Design notes ${index + 1}: keep search, recap and topic sources in the same preview.`,
  category: index % 3 === 0 ? '阅读学习' : '编程开发',
}));

export const source = (id = 1) => ({ id, timestamp_ms: snapshots[id - 1].captured_at_ms, process_name: snapshots[id - 1].process_name, window_title: snapshots[id - 1].window_title });

export const activities = [
  { id: 'review-search', task_id: 'ui-foundation', task_title: 'Review search and screenshot navigation',
    text: 'Compared the search entry, screenshot preview and return path. Keep the query and filters while opening sources.',
    start_ms: REVIEW_START, end_ms: REVIEW_START + 600000, sources: [source(1), source(2), source(3)], member_ids: [1, 2, 3],
    details: [{ text: 'Use one preview surface for search, recap and topic sources.', evidence_ids: [1, 2] }] },
  { id: 'review-reading', task_id: 'reading-layout', task_title: 'Plan recap and topic reading layouts',
    text: 'Collected examples for shorter overviews, readable line lengths and clearer links to source screenshots.',
    start_ms: REVIEW_START + 3600000, end_ms: REVIEW_START + 5400000, sources: [source(4), source(5)], member_ids: [4, 5] },
];

export const clusters = [{ id: 7, anchor_text: 'CarbonPaper interface design', display_name: 'CarbonPaper interface design',
  enabled: true, assignment_count: 24, recent_assignment_count: 3, dominant_color: '#007acc',
  updated_at: `${REVIEW_DATE}T11:00:00Z`, last_assigned_at: `${REVIEW_DATE}T10:00:00Z`, dominant_process_name: 'Editor',
  summary: { title: 'A consistent workspace for screenshot history', summary: 'Search, recap and topics each provide a way to find a screenshot. The preview keeps users in context and provides an explicit path to the timeline [1].\n\n## Reading and navigation\n\n- Keep the original query and selected filters.\n- Restore focus to the source after closing the preview.\n- Share the same recognized-text and source actions [2].',
    source_snapshot_count: 24, updated_at: `${REVIEW_DATE}T11:00:00Z`,
    evidence: [{ screenshot_id: 1, ref: 1, window_title: snapshots[0].window_title }, { screenshot_id: 2, ref: 2, window_title: snapshots[1].window_title }],
  },
}];
