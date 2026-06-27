const BROWSER_PROCESS_RE = /(chrome|msedge|edge|firefox|brave|chromium|browser|qqbrowser|360se)/i;

const SITE_NAME_MAP = new Map([
  ['github.com', 'GitHub'],
  ['gitlab.com', 'GitLab'],
  ['bitbucket.org', 'Bitbucket'],
  ['stackoverflow.com', 'Stack Overflow'],
  ['stackexchange.com', 'Stack Exchange'],
  ['developer.mozilla.org', 'MDN'],
  ['docs.rs', 'Docs.rs'],
  ['npmjs.com', 'npm'],
  ['pypi.org', 'PyPI'],
  ['notion.so', 'Notion'],
  ['figma.com', 'Figma'],
  ['youtube.com', 'YouTube'],
  ['youtu.be', 'YouTube'],
  ['bilibili.com', 'Bilibili'],
  ['zhihu.com', 'Zhihu'],
  ['weibo.com', 'Weibo'],
  ['x.com', 'X'],
  ['twitter.com', 'X'],
]);

const APP_NAME_MAP = new Map([
  ['code', 'Code'],
  ['code.exe', 'Code'],
  ['cursor', 'Cursor'],
  ['cursor.exe', 'Cursor'],
  ['chrome', 'Chrome'],
  ['chrome.exe', 'Chrome'],
  ['msedge', 'Edge'],
  ['msedge.exe', 'Edge'],
  ['firefox', 'Firefox'],
  ['firefox.exe', 'Firefox'],
  ['wechat', 'WeChat'],
  ['wechat.exe', 'WeChat'],
  ['weixin', 'WeChat'],
  ['weixin.exe', 'WeChat'],
  ['qq', 'QQ'],
  ['qq.exe', 'QQ'],
]);

const NOISY_PATH_SEGMENTS = new Set([
  'app',
  'dashboard',
  'explore',
  'feed',
  'home',
  'login',
  'notifications',
  'pulls',
  'search',
  'settings',
  'signin',
  'signup',
  'users',
]);

function compactText(value) {
  return String(value || '').replace(/\s+/g, ' ').trim();
}

function decodeSegment(segment) {
  try {
    return decodeURIComponent(segment.replace(/\+/g, ' '));
  } catch {
    return segment;
  }
}

function stripExtension(processName) {
  const clean = compactText(processName);
  return clean.replace(/\.exe$/i, '');
}

function titleCaseToken(token) {
  const clean = compactText(token).replace(/[-_]+/g, ' ');
  if (!clean) return '';
  if (clean.length <= 3) return clean.toUpperCase();
  return clean.replace(/\b\w/g, (ch) => ch.toUpperCase());
}

function rootHost(host) {
  const clean = String(host || '').toLowerCase().replace(/^(www|m|app)\./, '');
  const parts = clean.split('.').filter(Boolean);
  if (parts.length <= 2) return clean;
  return parts.slice(-2).join('.');
}

function isHostOrSubdomain(host, domain) {
  return host === domain || host.endsWith(`.${domain}`);
}

export function getHostname(url) {
  if (!url) return '';
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    try {
      return new URL(`https://${url}`).hostname.toLowerCase();
    } catch {
      return '';
    }
  }
}

export function normalizeProcessLabel(processName) {
  const clean = compactText(processName);
  if (!clean) return '';
  const lower = clean.toLowerCase();
  if (APP_NAME_MAP.has(lower)) return APP_NAME_MAP.get(lower);
  return titleCaseToken(stripExtension(clean));
}

export function isBrowserProcess(processName) {
  return BROWSER_PROCESS_RE.test(processName || '');
}

export function siteNameFromUrl(url) {
  const host = getHostname(url);
  if (!host) return { host: '', siteName: '' };
  const normalizedHost = host.replace(/^(www|m|app)\./, '');
  const root = rootHost(host);
  const mapped = SITE_NAME_MAP.get(normalizedHost) || SITE_NAME_MAP.get(root);
  if (mapped) return { host: normalizedHost, siteName: mapped };
  const first = normalizedHost.split('.')[0];
  return { host: normalizedHost, siteName: titleCaseToken(first) };
}

export function extractEntityFromUrl(url) {
  if (!url) return '';
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    try {
      parsed = new URL(`https://${url}`);
    } catch {
      return '';
    }
  }

  const host = parsed.hostname.toLowerCase().replace(/^(www|m|app)\./, '');
  const segments = parsed.pathname.split('/').map(decodeSegment).filter(Boolean);
  if (!segments.length) return '';

  if (
    isHostOrSubdomain(host, 'github.com')
    || isHostOrSubdomain(host, 'gitlab.com')
    || isHostOrSubdomain(host, 'bitbucket.org')
  ) {
    if (segments.length >= 2) return segments[1];
    return segments[0];
  }
  if (isHostOrSubdomain(host, 'npmjs.com') && segments[0] === 'package' && segments[1]) return segments[1];
  if (isHostOrSubdomain(host, 'pypi.org') && segments[0] === 'project' && segments[1]) return segments[1];
  if (isHostOrSubdomain(host, 'docs.rs') && segments[0]) return segments[0];

  const candidate = segments.find((seg) => {
    const lower = seg.toLowerCase();
    return !NOISY_PATH_SEGMENTS.has(lower)
      && /[\p{L}\p{N}]/u.test(seg)
      && seg.length >= 2
      && seg.length <= 48
      && !/^[0-9a-f]{16,}$/i.test(seg);
  });
  return compactText(candidate || '');
}

function stripKnownTitleSuffix(title, siteName, processName) {
  let clean = compactText(title);
  if (!clean) return '';

  const app = normalizeProcessLabel(processName);
  const suffixes = [
    app,
    siteName,
    'Google Chrome',
    'Microsoft Edge',
    'Mozilla Firefox',
    'Brave',
    'Chromium',
    'Visual Studio Code',
  ].filter(Boolean);

  for (const suffix of suffixes) {
    const escaped = suffix.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    clean = clean.replace(new RegExp(`\\s+[-|\\u00b7]\\s+${escaped}$`, 'i'), '').trim();
  }
  return clean;
}

export function extractEntityFromTitle(title, { siteName = '', processName = '' } = {}) {
  const clean = stripKnownTitleSuffix(title, siteName, processName);
  if (!clean) return '';

  const repoMatch = clean.match(/(?:^|[\s:/\u00b7])([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)/);
  if (repoMatch?.[2]) return repoMatch[2];

  const app = normalizeProcessLabel(processName);
  if (/^(Code|Cursor)$/i.test(app)) {
    const parts = clean.split(/\s+[-|]\s+/).map(compactText).filter(Boolean);
    const filtered = parts.filter((part) => !/^(Visual Studio Code|Code|Cursor)$/i.test(part));
    if (filtered.length >= 2) return filtered[filtered.length - 1];
  }

  const parts = clean.split(/\s+[-|\u00b7]\s+/).map(compactText).filter(Boolean);
  if (parts.length > 1) {
    const last = parts[parts.length - 1];
    if (last.length >= 2 && last.length <= 48 && !/^(Google Chrome|Microsoft Edge|Mozilla Firefox)$/i.test(last)) {
      return last;
    }
  }

  return clean.length > 64 ? `${clean.slice(0, 61)}...` : clean;
}

function mode(values) {
  const counts = new Map();
  for (const value of values.filter(Boolean)) {
    counts.set(value, (counts.get(value) || 0) + 1);
  }
  return [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || '';
}

function isWeakTaskLabel(label, processLabel) {
  const clean = compactText(label);
  if (!clean) return true;
  if (processLabel && clean.toLowerCase() === processLabel.toLowerCase()) return true;
  return /^(chrome|edge|firefox|browser-extension|code|cursor)$/i.test(clean);
}

export function buildActivityContext({ selectedEvent, selectedRecord, relatedResult } = {}) {
  const currentRecord = {
    screenshot_id: selectedEvent?.id,
    process_name: selectedRecord?.process_name || selectedEvent?.appName,
    window_title: selectedRecord?.window_title || selectedEvent?.windowTitle,
    page_url: selectedRecord?.page_url,
    category: selectedRecord?.category || selectedEvent?.category,
    timestamp: selectedRecord?.timestamp || (selectedEvent?.timestamp ? selectedEvent.timestamp / 1000 : null),
  };
  const related = Array.isArray(relatedResult?.screenshots) ? relatedResult.screenshots : [];
  const records = [currentRecord, ...related].filter((record) => record?.process_name || record?.window_title || record?.page_url);

  const currentSite = siteNameFromUrl(currentRecord.page_url);
  const dominantHost = currentSite.host || mode(records.map((record) => siteNameFromUrl(record.page_url).host));
  const dominantSite = currentSite.siteName || mode(records
    .filter((record) => siteNameFromUrl(record.page_url).host === dominantHost)
    .map((record) => siteNameFromUrl(record.page_url).siteName));

  const processLabel = normalizeProcessLabel(
    relatedResult?.dominant_process || mode(records.map((record) => record.process_name))
  );
  const browserLike = isBrowserProcess(relatedResult?.dominant_process || currentRecord.process_name || '');

  const urlEntity = extractEntityFromUrl(currentRecord.page_url)
    || mode(records
      .filter((record) => !dominantHost || siteNameFromUrl(record.page_url).host === dominantHost)
      .map((record) => extractEntityFromUrl(record.page_url)));
  const titleEntity = extractEntityFromTitle(currentRecord.window_title, {
    siteName: dominantSite,
    processName: currentRecord.process_name,
  }) || mode(records.map((record) => extractEntityFromTitle(record.window_title, {
    siteName: dominantSite,
    processName: record.process_name,
  })));
  const entity = compactText(urlEntity || titleEntity);

  let title = '';
  if (dominantSite && (browserLike || currentRecord.page_url)) {
    title = entity && entity.toLowerCase() !== dominantSite.toLowerCase()
      ? `${dominantSite} - ${entity}`
      : dominantSite;
  } else if (processLabel && entity && entity.toLowerCase() !== processLabel.toLowerCase()) {
    title = `${processLabel} - ${entity}`;
  } else if (!isWeakTaskLabel(relatedResult?.task_label, processLabel)) {
    title = relatedResult.task_label;
  } else {
    title = processLabel || entity || relatedResult?.task_label || '';
  }

  return {
    title: compactText(title),
    siteName: dominantSite,
    host: dominantHost,
    processLabel,
    entity,
    category: relatedResult?.dominant_category || currentRecord.category || '',
    snapshotCount: relatedResult?.snapshot_count || null,
    startTime: relatedResult?.start_time || null,
    endTime: relatedResult?.end_time || null,
    currentTimestamp: relatedResult?.current_timestamp || currentRecord.timestamp || null,
  };
}
