import { describe, expect, it } from 'vitest';
import { getSnapshotSourceOptions } from './snapshot_sources';

const documentRef = {
  application: 'word',
  kind: 'local_file',
  display_name: 'Report.docx',
  resumable: true,
};

describe('snapshot_sources', () => {
  it('returns the resumable document as the only direct source', () => {
    const sources = getSnapshotSourceOptions({
      documentRef,
      screenshotId: 42,
    });

    expect(sources).toHaveLength(1);
    expect(sources[0]).toMatchObject({ kind: 'office', documentRef });
  });

  it('offers both the document and page when both are available', () => {
    const sources = getSnapshotSourceOptions({
      documentRef,
      screenshotId: 42,
      pageUrl: 'https://example.com/work',
    });

    expect(sources.map((source) => source.kind)).toEqual(['office', 'url']);
    expect(sources[1].url).toBe('https://example.com/work');
  });

  it('uses OCR URLs when no page URL is available', () => {
    const sources = getSnapshotSourceOptions({
      screenshotId: 42,
      ocrResults: [
        { text: 'https://one.example/a and www.two.example/b' },
      ],
    });

    expect(sources.map((source) => source.url)).toEqual([
      'https://one.example/a',
      'https://www.two.example/b',
    ]);
  });

  it('does not expose an unsaved document as an actionable source', () => {
    const sources = getSnapshotSourceOptions({
      documentRef: { ...documentRef, resumable: false, kind: 'unsaved' },
      screenshotId: 42,
    });

    expect(sources).toEqual([]);
  });

  it('does not offer a document without a valid screenshot id', () => {
    expect(getSnapshotSourceOptions({
      documentRef,
      screenshotId: -1,
    })).toEqual([]);
  });
});
