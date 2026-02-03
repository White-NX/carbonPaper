const URL_REGEX = /((https?:\/\/|www\.)[^\s<>"{}|\\^`\[\]]+)/gi;

const normalizeUrl = (value) => {
  if (!value) return '';
  if (value.startsWith('http://') || value.startsWith('https://')) {
    return value;
  }
  if (value.startsWith('www.')) {
    return `https://${value}`;
  }
  return value;
};

export const extractUrlsFromOcr = (ocrResults) => {
  if (!ocrResults || !Array.isArray(ocrResults)) return [];

  const urls = new Set();

  for (const result of ocrResults) {
    const text = result?.text || '';
    const matches = text.match(URL_REGEX);
    if (matches) {
      matches.forEach((match) => {
        const normalized = normalizeUrl(match.trim());
        if (normalized) {
          urls.add(normalized);
        }
      });
    }
  }

  return Array.from(urls);
};

export const hasUrlsInOcr = (ocrResults) => extractUrlsFromOcr(ocrResults).length > 0;