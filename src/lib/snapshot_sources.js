import { extractUrlsFromOcr } from './ocr_url_detector';

/**
 * Build the actionable sources associated with a snapshot.  Office locators
 * remain backend-only; the public document reference is enough for display
 * and for deciding whether the authenticated resume command is available.
 */
export function getSnapshotSourceOptions({ documentRef, screenshotId, pageUrl, ocrResults }) {
  const urls = pageUrl
    ? [pageUrl]
    : extractUrlsFromOcr(ocrResults || []).slice(0, 5);
  const options = [];

  if (documentRef?.resumable && Number(screenshotId) > 0) {
    options.push({
      id: 'office-document',
      kind: 'office',
      documentRef,
    });
  }

  urls.forEach((url, index) => {
    if (!url) return;
    options.push({
      id: `url-${index}-${url}`,
      kind: 'url',
      url,
    });
  });

  return options;
}
