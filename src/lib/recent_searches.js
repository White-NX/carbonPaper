/**
 * 最近搜索记录。
 *
 * 只存查询词和检索模式，落在 localStorage 里，不经过后端也不落库。
 * 搜索页的着陆区靠它填充，用户可以一键重跑上次的查询。
 */

const STORAGE_KEY = 'carbonpaper.recentSearches';
const MAX_ENTRIES = 8;

function readRaw() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    // localStorage 不可用或内容损坏时当作没有历史，不影响搜索本身。
    return [];
  }
}

function writeRaw(entries) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // 写入失败（配额或隐私模式）不需要打扰用户。
  }
}

/**
 * 读取最近搜索，最新的排在最前。
 * @returns {{ query: string, mode: string, at: number }[]}
 */
export function loadRecentSearches() {
  return readRaw()
    .filter((entry) => entry && typeof entry.query === 'string' && entry.query.trim())
    .slice(0, MAX_ENTRIES);
}

/**
 * 记录一次搜索。同样的查询词只保留最新一条，并移到最前面。
 *
 * @param {string} query 查询词，空字符串会被忽略
 * @param {string} mode 检索模式（ocr / nl）
 * @returns {{ query: string, mode: string, at: number }[]} 更新后的列表
 */
export function pushRecentSearch(query, mode) {
  const trimmed = (query || '').trim();
  if (!trimmed) return loadRecentSearches();

  const rest = readRaw().filter(
    (entry) => entry && typeof entry.query === 'string' && entry.query.trim() !== trimmed
  );
  const next = [{ query: trimmed, mode: mode || 'ocr', at: Date.now() }, ...rest].slice(0, MAX_ENTRIES);
  writeRaw(next);
  return next;
}

/** 清空最近搜索。 */
export function clearRecentSearches() {
  writeRaw([]);
  return [];
}
