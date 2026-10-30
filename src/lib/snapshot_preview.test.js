import { describe, expect, it } from 'vitest';
import { getSnapshotPreviewKey, normalizeSnapshotPreviewItem, sanitizeSnapshotPreviewState, searchResultToSnapshotEvent } from './snapshot_preview';
import { captureDateOf } from './search_grouping';
import fixture from '../tests/fixtures/screenshot_contract_v3.json';

describe('snapshot preview persistence', () => {
  it.each([undefined, 'd766e83ee7354fb0d83f3dbf6d68c504'])('preserves the nested screenshot id through visual preview and persistence when the top-level screenshot id is %s', (screenshotId) => {
    const source = {
      id: 'd766e83ee7354fb0d83f3dbf6d68c504', screenshot_id: screenshotId, similarity: 0.8,
      metadata: { screenshot_id: 42, image_path: 'memory://visual', created_at: fixture.captured_at_utc },
    };
    const preview = normalizeSnapshotPreviewItem(source);
    expect(preview.screenshot_id).toBe(42);
    expect(preview.id).toBe(42);
    expect(getSnapshotPreviewKey(source)).toBe('id:42');
    const persisted = sanitizeSnapshotPreviewState({ tabs: [source], activeKey: 'id:42' });
    expect(persisted.activeKey).toBe('id:42');
    expect(persisted.tabs[0].metadata.screenshot_id).toBe(42);
    expect(searchResultToSnapshotEvent(persisted.tabs[0])).toMatchObject({ id: 42, path: 'memory://visual' });
  });

  it('uses the image path when a visual hit has only an index document id', () => {
    const source = { id: 'd766e83ee7354fb0d83f3dbf6d68c504', metadata: { image_path: 'memory://visual' } };
    const preview = normalizeSnapshotPreviewItem(source);
    expect(preview.screenshot_id).toBeNull();
    expect(getSnapshotPreviewKey(preview)).toBe('path:memory://visual');
    const persisted = sanitizeSnapshotPreviewState({ tabs: [preview] }).tabs[0];
    expect(persisted.screenshot_id).toBeNull();
    expect(persisted.metadata).not.toHaveProperty('screenshot_id');
    expect(searchResultToSnapshotEvent(persisted)).toMatchObject({ id: -1, path: 'memory://visual' });
    expect(searchResultToSnapshotEvent({ screenshot_id: source.id })).toBeNull();
  });

  it('keeps the tool capture instant through main preview, dock and standalone persistence', () => {
    const source = { screenshot_id: fixture.screenshot_id, captured_at_ms: fixture.captured_at_ms, created_at: fixture.ocr_processed_at_utc };
    const event = searchResultToSnapshotEvent(source);
    expect(event.id).toBe(fixture.screenshot_id);
    expect(event.timestamp).toBe(fixture.captured_at_ms);
    const dock = normalizeSnapshotPreviewItem(source);
    expect(dock.created_at).toBe(new Date(fixture.captured_at_ms).toISOString());
    const standalone = sanitizeSnapshotPreviewState({ tabs: [dock] }).tabs[0];
    expect(standalone.captured_at_ms).toBe(fixture.captured_at_ms);
    expect(captureDateOf(standalone).getTime()).toBe(event.timestamp);
    expect(standalone.created_at).not.toBe(fixture.ocr_processed_at_utc);
  });

  it('keeps unknown capture times unknown instead of selecting now or OCR completion', () => {
    const source = { screenshot_id: 42, captured_at_ms: null, created_at: fixture.ocr_processed_at_utc };
    expect(searchResultToSnapshotEvent(source).timestamp).toBeNull();
    const persisted = sanitizeSnapshotPreviewState({ tabs: [normalizeSnapshotPreviewItem(source)] }).tabs[0];
    expect(persisted.captured_at_ms).toBeNull();
    expect(persisted.created_at).toBeNull();
  });

  it('retains legacy seconds while treating explicitly named milliseconds literally', () => {
    expect(searchResultToSnapshotEvent({ screenshot_id: 42, timestamp: 1000 }).timestamp).toBe(1000000);
    expect(searchResultToSnapshotEvent({ screenshot_id: 42, captured_at_ms: 1000 }).timestamp).toBe(1000);
    expect(searchResultToSnapshotEvent({ screenshot_id: 42, captured_at_ms: 0 }).timestamp).toBe(0);
    expect(searchResultToSnapshotEvent({ ocr_id: 42 })).toBeNull();
  });

  it('removes base64 image fields from persisted tabs', () => {
    const state = sanitizeSnapshotPreviewState({
      activeKey: 'id:7',
      tabs: [
        {
          screenshot_id: 7,
          image_path: 'D:/shots/7.jpg',
          process_name: 'chrome.exe',
          window_title: 'Example',
          thumbnailSrc: 'data:image/jpeg;base64,thumb',
          imageUrl: 'data:image/png;base64,full',
          src: 'data:image/png;base64,other',
          ocr_text: 'large text',
          metadata: {
            screenshot_id: 7,
            image_path: 'D:/shots/7.jpg',
            process_name: 'chrome.exe',
            window_title: 'Example',
          },
        },
      ],
    });

    expect(state.activeKey).toBe('id:7');
    expect(JSON.stringify(state)).not.toContain('data:image');
    expect(state.tabs[0]).not.toHaveProperty('thumbnailSrc');
    expect(state.tabs[0]).not.toHaveProperty('imageUrl');
    expect(state.tabs[0]).not.toHaveProperty('src');
    expect(state.tabs[0]).not.toHaveProperty('ocr_text');
  });
});
