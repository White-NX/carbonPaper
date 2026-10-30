import { REVIEW_DATE, REVIEW_START, snapshots, activities, clusters, source } from './fixture.js';

const params = new URLSearchParams(location.search);
let authenticated = params.get('page') !== 'lock';
let paused = false;
let nextId = 0;
const listeners = new Map();
let imagePromise;
let thumbnailPromise;
const imageResponse = async () => {
  imagePromise ||= fetch('/tools/ui-review/fixtures/screenshot.svg').then((response) => response.text())
    .then((svg) => ({
      status: 'success', mime_type: 'image/svg+xml',
      data: btoa(String.fromCharCode(...new TextEncoder().encode(svg))),
    }));
  return imagePromise;
};
const thumbnailResponse = async () => {
  thumbnailPromise ||= imageResponse().then(async (response) => {
    const image = new Image();
    image.src = `data:${response.mime_type};base64,${response.data}`;
    await image.decode();
    const canvas = document.createElement('canvas');
    const ratio = 192 / Math.max(image.naturalWidth, image.naturalHeight);
    canvas.width = Math.round(image.naturalWidth * ratio);
    canvas.height = Math.round(image.naturalHeight * ratio);
    canvas.getContext('2d').drawImage(image, 0, 0, canvas.width, canvas.height);
    return { status: 'success', mime_type: 'image/jpeg', data: canvas.toDataURL('image/jpeg', 0.8).split(',')[1] };
  });
  return thumbnailPromise;
};

export async function listen(event, handler) {
  const id = ++nextId;
  listeners.set(id, { event, handler });
  return () => listeners.delete(id);
}
export async function emitTo(_target, event, payload) {
  for (const [id, entry] of listeners) if (entry.event === event) entry.handler({ id, event, payload });
}
export async function emit(event, payload) { return emitTo(null, event, payload); }
export async function once(event, handler) {
  const dispose = await listen(event, (payload) => { dispose(); handler(payload); });
  return dispose;
}
export class Channel { onmessage = () => {}; }
export function convertFileSrc(path) { return path; }
export async function openUrl(url) { console.info('UI review source action:', url); }
export const open = async () => null;
export const save = async () => null;

const mockWindow = {
  label: 'main', listen, once, emit, show: async () => {}, hide: async () => {}, setFocus: async () => {},
  minimize: async () => {}, toggleMaximize: async () => {}, isMaximized: async () => false,
  close: async () => {}, startDragging: async () => {},
};
export const getCurrentWindow = () => mockWindow;
export class WebviewWindow {
  constructor(label) { this.label = label; }
  static async getByLabel() { return null; }
  async once() { return () => {}; }
}

const noWork = { running: false, in_progress: false, needs_vacuum: false, phase: 'idle', manualActive: false };
export async function invoke(command, args = {}) {
  const counts = globalThis.__uiReviewCommandCounts ||= {};
  counts[command] = (counts[command] || 0) + 1;
  switch (command) {
    case 'credential_check_session': return authenticated;
    case 'credential_get_session_timeout': return 900;
    case 'credential_initialize': return true;
    case 'credential_verify_user': authenticated = true; await emit('auth-session-changed', true); return true;
    case 'credential_lock_session': authenticated = false; await emit('auth-session-changed', false); return null;
    case 'get_monitor_autostart': return false;
    case 'get_monitor_status': return JSON.stringify({ paused, stopped: false, running: true, power_saving_active: false });
    case 'pause_monitor': paused = true; return null;
    case 'resume_monitor': paused = false; return null;
    case 'get_power_saving_status': return { enabled: false, active: false };
    case 'get_onboarding_state': return { mode: 'none', current_version: 1, completed_version: 1 };
    case 'check_model_files': return {};
    case 'get_critical_errors': return [];
    case 'storage_check_hmac_migration_status': return { needs_migration: false, is_running: false };
    case 'get_blind_index_repair_status': return { needs_repair: false };
    case 'get_clip_backfill_offer': return { should_offer: false };
    case 'get_maintenance_status': return { active: false };
    case 'app_bound_status': return { offer_enable: false };
    case 'storage_get_startup_vacuum_status': return noWork;
    case 'storage_get_thumbnail_warmup_status': case 'storage_warmup_thumbnails': return noWork;
    case 'storage_get_timeline': {
      const toMilliseconds = (value) => value > 10_000_000_000 ? value : value * 1000;
      const records = snapshots.filter((item) => item.captured_at_ms >= toMilliseconds(args.startTime)
        && item.captured_at_ms <= toMilliseconds(args.endTime));
      const sampleStep = Math.max(1, Number(params.get('timelineSampleStep')) || 1);
      return records.filter((_item, index) => index % sampleStep === 0).slice(0, args.maxRecords || 500);
    }
    case 'storage_get_timeline_details': return snapshots.filter((item) => args.ids.includes(item.id));
    case 'storage_get_timeline_index': {
      const ordered = [...snapshots].sort((a, b) => a.captured_at_ms - b.captured_at_ms || a.id - b.id);
      const before = ordered.filter((item) => item.captured_at_ms < args.startTimeMs).at(-1);
      const after = ordered.find((item) => item.captured_at_ms > args.endTimeMs);
      return [before, ...ordered.filter((item) => item.captured_at_ms >= args.startTimeMs
        && item.captured_at_ms <= args.endTimeMs), after].filter(Boolean)
        .map((item) => [item.id, item.captured_at_ms]);
    }
    case 'storage_get_timeline_nearest': {
      const nearest = snapshots.reduce((best, item) => !best
        || Math.abs(item.captured_at_ms - args.timeMs) < Math.abs(best.captured_at_ms - args.timeMs) ? item : best, null);
      return nearest ? [nearest] : [];
    }
    case 'storage_get_timeline_density': return Array.from({ length: 16 }, (_, index) => ({ timestamp: REVIEW_START / 1000 + index * 300, count: index % 5 + 2 }));
    case 'storage_get_image': {
      const delay = Number(params.get('imageDelayMs')) || 0;
      if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay));
      return imageResponse();
    }
    case 'storage_get_thumbnail': return thumbnailResponse();
    case 'storage_batch_get_thumbnails': return { results: Object.fromEntries(await Promise.all(args.ids.map(async (id) => [id, await thumbnailResponse()]))) };
    case 'storage_get_screenshot_details': return {
      record: { ...snapshots[(args.id || 1) - 1], status: 'ready', visible_links: [] },
      ocr_results: [
        { id: 1, text: 'A familiar place for every screenshot', box_coords: [[290, 95], [1160, 95], [1160, 140], [290, 140]] },
        { id: 2, text: 'Find a moment. Open its source. Return to where you were.' },
        { id: 3, text: 'Search keeps the query and filters.' },
        { id: 4, text: 'Recap keeps the date and reading position.' },
        { id: 5, text: 'Topics keep the selected collection and citation.' },
      ],
    };
    case 'storage_list_recent_screenshots': return snapshots;
    case 'storage_list_processes': return ['Browser', 'Editor', 'Notes'].map((process_name) => ({ process_name, count: 8 }));
    case 'storage_get_categories_from_db': return ['阅读学习', '编程开发'];
    case 'storage_batch_get_categories': return {};
    case 'storage_search': {
      const query = (args.query || '').trim().toLowerCase();
      const matches = snapshots.filter((snapshot) => `${snapshot.text} ${snapshot.window_title} ${snapshot.process_name}`.toLowerCase().includes(query));
      const offset = args.offset || 0;
      return matches.slice(offset, offset + (args.limit || 20));
    }
    case 'storage_search_unified': return {
      items: snapshots.slice(0, 6).map((snapshot) => ({ kind: 'snapshot', snapshot })), total: 6, snapshot_count: 6,
      recap_count: 0, next_offset: null, semantic_status: 'disabled', complete: true,
    };
    case 'monitor_search_nl': return { results: snapshots.slice(0, 12).map((snapshot) => ({ ...snapshot, metadata: snapshot, ocr_text: snapshot.text, similarity: 0.85 })) };
    case 'recap_get_settings': return { enabled: true };
    case 'recap_list_days': return [REVIEW_DATE];
    case 'recap_get_day': return {
      date: args.date, calendar: { time_zone: 'Asia/Shanghai' }, running: false, can_undo: false,
      batches: activities.map((activity, index) => ({ start_ms: REVIEW_START + index * 14400000, end_ms: REVIEW_START + (index + 1) * 14400000,
        status: 'ready', updated_at_ms: REVIEW_START + 7200000,
        summary: { overview: index === 0 ? 'Reviewed how people move between search, screenshots and their original sources.' : 'Collected the next round of reading-layout improvements.',
          topics: [{ task_id: activity.task_id, title: activity.task_title, text: activity.text }] },
        activities: [activity], apps: [],
      })),
      threads: activities.map((activity) => ({ id: activity.task_id, title: activity.task_title, activity_ids: [activity.id] })),
    };
    case 'recap_get_records': return { items: snapshots.map((item) => source(item.id)), total: snapshots.length, next_cursor: null };
    case 'recap_get_snapshot_context': return { status: 'ready', items: [{ ...activities[0], date: REVIEW_DATE, activity_id: activities[0].id }] };
    case 'recap_get_progress': return null;
    case 'smart_cluster_list': return clusters;
    case 'smart_cluster_assignments': return snapshots.map((snapshot) => ({ screenshot_id: snapshot.id, assigned_at: snapshot.created_at, process_name: snapshot.process_name, window_title: snapshot.window_title }));
    case 'smart_cluster_status': return { enabled_cluster_count: 1, pending_count: 0 };
    case 'monitor_smart_cluster_worker_status': return noWork;
    case 'get_background_index_progress': return noWork;
    case 'storage_compute_link_scores': return [];
    default: return null;
  }
}

// Keep review controls on the same module instance as the connected event listeners.
globalThis.__uiReviewInvoke = invoke;
