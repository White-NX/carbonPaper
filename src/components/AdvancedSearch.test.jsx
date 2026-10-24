import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => {
      if (options?.returnObjects) {
        return ['hint-a', 'hint-b'];
      }
      if (key === 'advancedSearch.recapRelevance') return key + ':' + options.score;
      return key;
    },
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock('./ThumbnailCard', () => ({
  ThumbnailCard: ({ item, onSelect, imageKind = 'thumbnail' }) => (
    <button data-image-kind={imageKind} onClick={() => onSelect(item)}>{item.id || item.screenshot_id || 'thumb'}</button>
  ),
  CategoryBadge: () => <span>badge</span>,
}));

vi.mock('../lib/monitor_api', () => ({
  searchScreenshots: vi.fn(async () => []),
  fetchImage: vi.fn(async () => null),
  fetchSearchImage: vi.fn(async () => null),
  fetchThumbnail: vi.fn(async () => null),
  fetchThumbnailBatch: vi.fn(async () => ({})),
  listProcesses: vi.fn(async () => [{ process_name: 'code.exe', count: 2 }]),
  listRecentScreenshots: vi.fn(async () => []),
  getCategoriesFromDb: vi.fn(async () => ['编程开发']),
  batchGetCategories: vi.fn(async () => ({})),
}));

import { AdvancedSearch } from './AdvancedSearch';
import { searchSnapshotsAndRecaps } from '../lib/search_api';
vi.mock('../lib/search_api', () => ({ searchSnapshotsAndRecaps: vi.fn() }));
const response = (snapshots, offset = 0) => ({
  items: snapshots.map((snapshot) => ({ kind: 'snapshot', snapshot })),
  total: snapshots.length, snapshot_count: snapshots.length, recap_count: 0,
  next_offset: snapshots.length === 40 ? offset + 40 : null,
  semantic_status: 'disabled',
});
import {
  searchScreenshots,
  listProcesses,
  listRecentScreenshots,
  getCategoriesFromDb,
  fetchThumbnailBatch,
} from '../lib/monitor_api';

const makeOcrResult = (id) => ({
  screenshot_id: id,
  process_name: 'code.exe',
  text: `text-${id}`,
  window_title: `window-${id}`,
  timestamp: 1_754_900_000 - id,
});
const recapHit = {
  date: '2026-10-09', activity_id: 'activity', task_id: 'task', task_title: 'Recall the design decision',
  text: 'We compared two indexing strategies.', details: [{ text: 'Keep local search fast', evidence_ids: [99] }],
  start_ms: 1791504000000, end_ms: 1791507600000,
  sources: [{ id: 99, timestamp_ms: 1791504000000, process_name: 'code.exe', window_title: 'Design notes' }],
};
const mixedResponse = (semanticStatus = 'disabled') => ({
  ...response([makeOcrResult(1)]),
  items: [{ kind: 'snapshot', snapshot: makeOcrResult(1) }, { kind: 'recap', recap: recapHit }],
  total: 2, snapshot_count: 1, recap_count: 1, semantic_status: semanticStatus,
});

describe('AdvancedSearch', () => {
  let observerCallback;

  beforeEach(() => {
    observerCallback = null;
    searchSnapshotsAndRecaps.mockReset().mockResolvedValue(response([]));
    globalThis.IntersectionObserver = class {
      constructor(callback) {
        observerCallback = callback;
      }

      observe() {}

      unobserve() {}

      disconnect() {}
    };

    invoke.mockImplementation((command) => {
      if (command === 'storage_check_hmac_migration_status') {
        return Promise.resolve({ needs_migration: false, is_running: false });
      }
      return Promise.resolve(null);
    });
  });

  it('defaults to mixed semantic search when vectors are ready', async () => {
    searchSnapshotsAndRecaps.mockResolvedValue(mixedResponse('ready'));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    expect(screen.getByRole('switch', { name: 'advancedSearch.recapSemantic' })).toHaveAttribute('aria-checked', 'true');
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('design', expect.objectContaining({ target: 'mixed', includeSemantic: true })));
  });

  it('changes search targets and hides snapshot-only filters from recaps', async () => {
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => Promise.resolve(options.target === 'recaps'
      ? { ...mixedResponse(), items: [{ kind: 'recap', recap: recapHit }], snapshot_count: 0, total: 1 }
      : response([makeOcrResult(1)])));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.targets.mixed' }));
    fireEvent.click(screen.getByRole('radio', { name: 'advancedSearch.targets.snapshots' }));
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('design', expect.objectContaining({ target: 'snapshots', includeSemantic: false })));
    expect(screen.queryByRole('switch', { name: 'advancedSearch.recapSemantic' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.targets.snapshots' }));
    fireEvent.click(screen.getByRole('radio', { name: 'advancedSearch.targets.recaps' }));
    await screen.findByRole('button', { name: /Recall the design decision/ });
    expect(screen.queryByText('text-1')).not.toBeInTheDocument();
    expect(screen.queryByText('advancedSearch.categories.all')).not.toBeInTheDocument();
    expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('design', expect.objectContaining({ target: 'recaps', categories: [], includeSemantic: false }));
  });

  it('shows relevance only for recap vector matches and drops late results after disabling vectors', async () => {
    let resolveSemantic;
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => options.includeSemantic
      ? new Promise((resolve) => { resolveSemantic = resolve; }) : Promise.resolve(mixedResponse('ready')));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    const toggle = screen.getByRole('switch', { name: 'advancedSearch.recapSemantic' });
    await screen.findByText('advancedSearch.semanticUpdating');
    const vectorResponse = { ...mixedResponse('ready'), items: [{ kind: 'recap', recap: { ...recapHit, semantic_score: 0.83 } }] };
    await act(async () => resolveSemantic(vectorResponse));
    expect(await screen.findByText('advancedSearch.recapRelevance:0.83')).toBeInTheDocument();
    fireEvent.click(toggle);
    await screen.findByText('text-1');
    expect(screen.queryByText('advancedSearch.recapRelevance:0.83')).not.toBeInTheDocument();
    fireEvent.click(toggle);
    await screen.findByText('advancedSearch.semanticUpdating');
    fireEvent.click(toggle);
    await screen.findByText('text-1');
    await act(async () => resolveSemantic(vectorResponse));
    expect(screen.queryByText('advancedSearch.recapRelevance:0.83')).not.toBeInTheDocument();
  });

  it('keeps the selected target and vector mode while loading another page', async () => {
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => Promise.resolve({
      ...response([]), items: [{ kind: 'recap', recap: { ...recapHit, activity_id: `activity-${options.offset || 0}`, semantic_score: options.includeSemantic ? 0.76 : null } }],
      next_offset: options.offset ? null : 40, total: 41, recap_count: 41, semantic_status: 'ready', ranking_revision: 'a'.repeat(64),
    }));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByRole('button', { name: /Recall the design decision/ });
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.targets.mixed' }));
    fireEvent.click(screen.getByRole('radio', { name: 'advancedSearch.targets.recaps' }));
    await screen.findByText('advancedSearch.recapRelevance:0.76');
    await act(async () => { observerCallback?.([{ isIntersecting: true }]); });
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('design', expect.objectContaining({
      target: 'recaps', includeSemantic: true, offset: 40, rankingRevision: 'a'.repeat(64),
    })));
    expect(screen.getAllByRole('button', { name: /Recall the design decision/ })).toHaveLength(2);
  });

  it('loads filter options when active', async () => {
    render(
      <AdvancedSearch
        active
        searchParams={{ query: '', mode: 'ocr' }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(listProcesses).toHaveBeenCalledTimes(1);
      expect(getCategoriesFromDb).toHaveBeenCalledTimes(1);
    });
  });

  it('shows mixed results, activity details, source screenshots and the daily recap route', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce(mixedResponse());
    const onOpenRecapDay = vi.fn();
    const onSelectResult = vi.fn();
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} onSelectResult={onSelectResult} onOpenRecapDay={onOpenRecapDay} />);
    expect(await screen.findByText('text-1')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Recall the design decision/ })).toBeInTheDocument();
    fireEvent.click(screen.getByText('advancedSearch.viewSource'));
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 99 }));
    fireEvent.click(screen.getByRole('button', { name: /Recall the design decision/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('Keep local search fast');
    fireEvent.click(within(screen.getByRole('dialog')).getByText('recap.search.openDay'));
    expect(onOpenRecapDay).toHaveBeenCalledWith('2026-10-09');
  });

  it('explains keyword-only, approximate and combined recap matches', async () => {
    const recaps = [
      { ...recapHit, activity_id: 'literal', task_title: 'CarbonPaper settings', keyword_rank: 1, semantic_score: null, match_evidence: { match_type: 'literal' } },
      { ...recapHit, activity_id: 'approximate', task_title: 'CarbonPapr release', keyword_rank: 2, semantic_score: null, match_evidence: { match_type: 'approximate' } },
      { ...recapHit, activity_id: 'both', task_title: 'CarbonPaper history', keyword_rank: 3, semantic_score: 0.83, match_evidence: { match_type: 'literal' } },
      { ...recapHit, activity_id: 'semantic', task_title: 'Review desktop capture choices', keyword_rank: null, semantic_score: 0.65 },
    ];
    searchSnapshotsAndRecaps.mockResolvedValue({
      ...response([]), items: recaps.map((recap) => ({ kind: 'recap', recap })),
      total: 4, recap_count: 4, semantic_status: 'ready',
    });
    render(<AdvancedSearch active searchParams={{ query: 'carbonpaper', mode: 'ocr' }} />);
    const literal = within((await screen.findByRole('button', { name: /CarbonPaper settings/ })).closest('article'));
    expect(literal.getByText('advancedSearch.recapKeywordMatch')).toBeInTheDocument();
    expect(literal.queryByText(/advancedSearch.recapRelevance:/)).not.toBeInTheDocument();
    const approximate = within(screen.getByRole('button', { name: /CarbonPapr release/ }).closest('article'));
    expect(approximate.getByText('advancedSearch.recapApproximateKeywordMatch')).toBeInTheDocument();
    const both = within(screen.getByRole('button', { name: /CarbonPaper history/ }).closest('article'));
    expect(both.getByText('advancedSearch.recapKeywordMatch')).toBeInTheDocument();
    expect(both.getByText('advancedSearch.recapRelevance:0.83')).toBeInTheDocument();
    const semantic = within(screen.getByRole('button', { name: /Review desktop capture choices/ }).closest('article'));
    expect(semantic.getByText('advancedSearch.recapRelevance:0.65')).toBeInTheDocument();
    expect(semantic.queryByText('advancedSearch.recapKeywordMatch')).not.toBeInTheDocument();
  });

  it('renders keywords while semantics load and replaces the first page when ready', async () => {
    let resolveSemantic;
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => options.includeSemantic
      ? new Promise((resolve) => { resolveSemantic = resolve; })
      : Promise.resolve({ ...response([makeOcrResult(1)]), semantic_status: 'ready' }));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    expect(await screen.findByText('text-1')).toBeInTheDocument();
    await screen.findByText('advancedSearch.semanticUpdating');
    await act(async () => resolveSemantic(mixedResponse('ready')));
    expect(screen.getByRole('button', { name: /Recall the design decision/ })).toBeInTheDocument();
    expect(screen.queryByText('advancedSearch.semanticUpdating')).not.toBeInTheDocument();
    expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('design', expect.objectContaining({ includeSemantic: true }));
  });

  it('ignores late semantic responses after the user changes the query', async () => {
    let resolveSemantic;
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => options.includeSemantic
      ? new Promise((resolve) => { resolveSemantic = resolve; })
      : Promise.resolve({ ...response([makeOcrResult(1)]), semantic_status: 'ready' }));
    render(<AdvancedSearch active searchParams={{ query: 'old', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    await screen.findByText('advancedSearch.semanticUpdating');
    fireEvent.change(screen.getByPlaceholderText('advancedSearch.search.placeholder_ocr'), { target: { value: 'new' } });
    await act(async () => resolveSemantic(mixedResponse('ready')));
    expect(screen.queryByRole('button', { name: /Recall the design decision/ })).not.toBeInTheDocument();
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('new', expect.anything()));
  });

  it('keeps an open activity detail when semantic ranking replaces the same activity', async () => {
    let resolveSemantic;
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => options.includeSemantic
      ? new Promise((resolve) => { resolveSemantic = resolve; })
      : Promise.resolve(mixedResponse('ready')));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByRole('button', { name: /Recall the design decision/ });
    await screen.findByText('advancedSearch.semanticUpdating');
    fireEvent.click(await screen.findByRole('button', { name: /Recall the design decision/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('Keep local search fast');
    await act(async () => resolveSemantic(structuredClone(mixedResponse('ready'))));
    expect(screen.getByRole('dialog')).toHaveTextContent('Keep local search fast');
  });

  it('keeps old pages when a later page fails to load', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    searchSnapshotsAndRecaps.mockResolvedValueOnce(response(Array.from({ length: 40 }, (_, i) => makeOcrResult(i + 1))))
      .mockRejectedValueOnce(new Error('Database busy'));
    render(<AdvancedSearch active searchParams={{ query: 'page', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    await act(async () => { observerCallback?.([{ isIntersecting: true }]); });
    await screen.findByText('advancedSearch.search.error');
    expect(screen.getByText('text-1')).toBeInTheDocument();
    consoleError.mockRestore();
  });

  it('keeps keyword results when semantic inference is unavailable', async () => {
    searchSnapshotsAndRecaps.mockImplementation((_query, options) => options.includeSemantic
      ? Promise.reject(new Error('RECAP_SEARCH_MODEL_UNAVAILABLE'))
      : Promise.resolve({ ...response([makeOcrResult(1)]), semantic_status: 'ready' }));
    render(<AdvancedSearch active searchParams={{ query: 'design', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    await screen.findByText('advancedSearch.semanticFallback');
    expect(screen.getByText('text-1')).toBeInTheDocument();
  });

  it('clears activity content and ignores pending responses after locking', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce(mixedResponse());
    const { rerender } = render(<AdvancedSearch active isAuthenticated searchParams={{ query: 'design', mode: 'ocr' }} />);
    fireEvent.click(await screen.findByRole('button', { name: /Recall the design decision/ }));
    await screen.findByRole('dialog');
    rerender(<AdvancedSearch active isAuthenticated={false} searchParams={{ query: 'design', mode: 'ocr' }} />);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Recall the design decision/ })).not.toBeInTheDocument();
  });

  it.each(['ocr', 'nl'])('keeps the %s landing grid on recent-capture thumbnails', async (mode) => {
    // 空查询会落进搜索的 OCR 回退路径，那条路径数的是文本块而不是截图。
    listRecentScreenshots.mockResolvedValueOnce([
      { screenshot_id: 91, process_name: 'terminal.exe', created_at: 1_754_900_000 },
      { screenshot_id: 92, process_name: 'terminal.exe', created_at: 1_754_899_970 },
      { screenshot_id: 93, process_name: 'msedge.exe', created_at: 1_754_899_940 },
    ]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: '', mode }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(fetchThumbnailBatch).toHaveBeenCalledWith([91, 92, 93]);
    });
    expect(searchScreenshots).not.toHaveBeenCalledWith('', mode, expect.anything());
  });

  it('runs OCR search with debounced query from searchParams', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce(response([{ screenshot_id: 1, text: 'hello world' }]));

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'hello', mode: 'ocr' }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(searchSnapshotsAndRecaps).toHaveBeenCalledWith('hello', expect.objectContaining({
        limit: 40,
        processNames: [],
      }));
    });
  });

  it('starts one unified request per query, filter, or explicit refresh', async () => {
    searchSnapshotsAndRecaps.mockResolvedValue(response([]));
    const props = {
      active: true,
      searchParams: { query: 'hello', mode: 'ocr', refreshKey: 1 },
      onSelectResult: vi.fn(),
    };
    const { rerender } = render(<AdvancedSearch {...props} />);
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenCalledTimes(1));

    const next = { ...props, searchParams: { ...props.searchParams, query: 'world' } };
    rerender(<AdvancedSearch {...next} />);
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith(
      'world', expect.objectContaining({ limit: 40 })
    ));
    expect(searchSnapshotsAndRecaps).toHaveBeenCalledTimes(2);

    fireEvent.click(screen.getByText('advancedSearch.processes.all'));
    const checkbox = within(screen.getByTestId('filter-panel'))
      .getByText('code.exe').closest('label').querySelector('input[type="checkbox"]');
    fireEvent.click(checkbox);
    fireEvent.mouseDown(document.body);
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenCalledTimes(3));
    expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith(
      'world', expect.objectContaining({ processNames: ['code.exe'] })
    );

    rerender(<AdvancedSearch {...next} searchParams={{ ...next.searchParams, refreshKey: 2 }} />);
    await waitFor(() => expect(searchSnapshotsAndRecaps).toHaveBeenCalledTimes(4));
    expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith(
      'world', expect.objectContaining({ processNames: ['code.exe'] })
    );
  });

  it('keeps visual mode available when the monitor is offline', async () => {
    const onSearchModeChange = vi.fn();

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'q', mode: 'nl' }}
        searchMode="nl"
        onSearchModeChange={onSearchModeChange}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => expect(screen.getByPlaceholderText('advancedSearch.search.placeholder_nl')).toBeInTheDocument());
    expect(onSearchModeChange).not.toHaveBeenCalled();
  });

  it('uses original cards for picture results without prefetching thumbnail batches', async () => {
    searchScreenshots.mockResolvedValueOnce([{ ...makeOcrResult(22), similarity: 0.8 }]);
    render(<AdvancedSearch active searchParams={{ query: 'moon', mode: 'nl' }} onSelectResult={vi.fn()} />);
    expect(await screen.findByRole('button', { name: '22' })).toHaveAttribute('data-image-kind', 'original');
    expect(fetchThumbnailBatch).not.toHaveBeenCalled();
  });

  it('shows no-result state when query exists but no matches', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce(response([]));

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'missing', mode: 'ocr' }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(screen.getByText('advancedSearch.search.no_results_for')).toBeInTheDocument();
    });
  });

  it('sends process/category/time filters in OCR search options', async () => {
    searchScreenshots.mockResolvedValue([]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: '', mode: 'ocr' }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(listProcesses).toHaveBeenCalledTimes(1);
    });

    // 筛选器收在药丸的下拉面板里，逐个展开操作，操作完点开外部把面板收起来。
    fireEvent.click(screen.getByText('advancedSearch.processes.all'));
    const processCheckbox = within(screen.getByTestId('filter-panel'))
      .getByText('code.exe')
      .closest('label')
      ?.querySelector('input[type="checkbox"]');
    expect(processCheckbox).not.toBeNull();
    fireEvent.click(processCheckbox);
    fireEvent.mouseDown(document.body);

    fireEvent.click(screen.getByText('advancedSearch.categories.all'));
    const categoryCheckbox = within(screen.getByTestId('filter-panel'))
      .getByText('编程开发')
      .closest('label')
      ?.querySelector('input[type="checkbox"]');
    expect(categoryCheckbox).not.toBeNull();
    fireEvent.click(categoryCheckbox);
    fireEvent.mouseDown(document.body);

    fireEvent.click(screen.getByText('advancedSearch.range.label'));
    fireEvent.change(document.querySelectorAll('input[type="datetime-local"]')[0], {
      target: { value: '2026-01-02T03:04' },
    });
    fireEvent.change(document.querySelectorAll('input[type="datetime-local"]')[1], {
      target: { value: '2026-01-02T04:05' },
    });

    await waitFor(() => {
      expect(searchSnapshotsAndRecaps).toHaveBeenCalledWith('', expect.objectContaining({
        processNames: ['code.exe'],
        categories: ['编程开发'],
        startTime: Math.floor(new Date('2026-01-02T03:04').getTime() / 1000),
        endTime: Math.floor(new Date('2026-01-02T04:05').getTime() / 1000),
      }));
    });
  });

  it('loads more results when sentinel intersects', async () => {
    searchSnapshotsAndRecaps
      .mockResolvedValueOnce(response(Array.from({ length: 40 }, (_, i) => makeOcrResult(i + 1))))
      .mockResolvedValueOnce(response([makeOcrResult(41)], 40));

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'page', mode: 'ocr' }}
        onSelectResult={vi.fn()}
      />
    );

    await waitFor(() => {
      expect(searchSnapshotsAndRecaps).toHaveBeenCalledWith('page', expect.objectContaining({
        limit: 40,
        processNames: [],
      }));
    });

    await act(async () => {
      observerCallback?.([{ isIntersecting: true }]);
    });

    await waitFor(() => {
      expect(searchSnapshotsAndRecaps).toHaveBeenCalledWith('page', expect.objectContaining({
        limit: 40,
        offset: 40,
      }));
    });

    expect(screen.getByText('text-41')).toBeInTheDocument();
  });

  it('replaces the old prefix and continues when a cached search is rebuilt', async () => {
    const firstPage = { ...response([makeOcrResult(1)]), next_offset: 40, ranking_revision: 'a'.repeat(64) };
    searchSnapshotsAndRecaps.mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce({ ...response([makeOcrResult(2)]), offset: 0, next_offset: 40, ranking_revision: 'b'.repeat(64) })
      .mockResolvedValueOnce({ ...response([makeOcrResult(3)]), offset: 40, next_offset: null, ranking_revision: 'b'.repeat(64) });
    render(<AdvancedSearch active searchParams={{ query: 'page', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    await act(async () => { observerCallback?.([{ isIntersecting: true }]); });
    await screen.findByText('text-2');
    expect(screen.queryByText('text-1')).not.toBeInTheDocument();
    expect(screen.queryByText('advancedSearch.search.error')).not.toBeInTheDocument();
    await act(async () => { observerCallback?.([{ isIntersecting: true }]); });
    await screen.findByText('text-3');
    expect(screen.getByText('text-2')).toBeInTheDocument();
    expect(searchSnapshotsAndRecaps).toHaveBeenLastCalledWith('page', expect.objectContaining({
      offset: 40, rankingRevision: 'b'.repeat(64),
    }));
  });

  it('deduplicates overlapping pages without stopping continuation', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce({ ...response([makeOcrResult(1)]), next_offset: 40 })
      .mockResolvedValueOnce({ ...response([makeOcrResult(1), makeOcrResult(2)]), offset: 40, next_offset: null });
    render(<AdvancedSearch active searchParams={{ query: 'page', mode: 'ocr' }} />);
    await screen.findByText('text-1');
    await act(async () => { observerCallback?.([{ isIntersecting: true }]); });
    await screen.findByText('text-2');
    expect(screen.getAllByText('text-1')).toHaveLength(1);
    expect(screen.queryByText('advancedSearch.search.error')).not.toBeInTheDocument();
  });

  it('publishes loaded OCR markers and highlights a hovered result', async () => {
    searchSnapshotsAndRecaps.mockResolvedValueOnce(response([makeOcrResult(11), makeOcrResult(12)]));
    const onTimelineSearchChange = vi.fn();

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'page', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        onTimelineSearchChange={onTimelineSearchChange}
      />
    );

    await waitFor(() => {
      expect(onTimelineSearchChange).toHaveBeenCalledWith(expect.objectContaining({
        markers: expect.arrayContaining([
          expect.objectContaining({ id: 'screenshot:11' }),
          expect.objectContaining({ id: 'screenshot:12' }),
        ]),
        hoveredIds: [],
      }));
    });

    fireEvent.mouseEnter(screen.getByText('text-11').closest('.group'));

    await waitFor(() => {
      expect(onTimelineSearchChange).toHaveBeenLastCalledWith(expect.objectContaining({
        hoveredIds: ['screenshot:11'],
      }));
    });

    fireEvent.mouseLeave(screen.getByText('text-11').closest('.group'));
    await waitFor(() => {
      expect(onTimelineSearchChange).toHaveBeenLastCalledWith(expect.objectContaining({
        hoveredIds: [],
      }));
    });
  });

  it('does not publish markers for visual search', async () => {
    searchScreenshots.mockResolvedValueOnce([{ ...makeOcrResult(21), similarity: 0.8 }]);
    const onTimelineSearchChange = vi.fn();

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'page', mode: 'nl' }}
        searchMode="nl"
        onSelectResult={vi.fn()}
        onTimelineSearchChange={onTimelineSearchChange}
      />
    );

    await waitFor(() => expect(searchScreenshots).toHaveBeenCalled());
    await waitFor(() => expect(onTimelineSearchChange).toHaveBeenLastCalledWith(null));
  });

  it('displays search error banner when search fails', async () => {
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    searchSnapshotsAndRecaps.mockRejectedValueOnce(new Error('Storage is offline'));

    try {
      render(
        <AdvancedSearch
          active
          searchParams={{ query: 'hello', mode: 'ocr' }}
          onSelectResult={vi.fn()}
        />
      );

      await waitFor(() => {
        expect(screen.getByText('advancedSearch.search.error')).toBeInTheDocument();
      });
      expect(consoleErrorSpy).toHaveBeenCalledWith(
        'Advanced search resetAndFetch failed:',
        expect.any(Error)
      );
    } finally {
      consoleErrorSpy.mockRestore();
    }
  });
});
