import { describe, expect, it } from 'vitest';
import { sanitizeSnapshotPreviewState } from './snapshot_preview';

describe('snapshot preview persistence', () => {
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
