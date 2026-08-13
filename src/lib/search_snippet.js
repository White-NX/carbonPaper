/**
 * OCR 摘要的规整与上下文截取。
 *
 * 搜索结果里的 OCR 文本是按识别块存储的多行字符串，直接渲染既丢上下文
 * 又会带进大量空行。这里负责把它压成适合单行展示的形式，并以命中词
 * 为中心开一个窗口，让用户看得出关键词出现在什么语境里。
 */

const ELLIPSIS = '…';

/** 中日韩字符区间：这些字符之间不需要用空格连接。 */
const CJK_RANGES = [
  [0x3040, 0x30ff], // 日文假名
  [0x3400, 0x4dbf], // 扩展 A
  [0x4e00, 0x9fff], // 基本区
  [0xf900, 0xfaff], // 兼容表意文字
  [0xff00, 0xffef], // 全角标点
];

function isCjk(char) {
  if (!char) return false;
  const code = char.codePointAt(0);
  return CJK_RANGES.some(([low, high]) => code >= low && code <= high);
}

/**
 * 把多行 OCR 文本压成一行。
 *
 * 一律用空格拼接会在中文之间插进空隙（"上海 地铁 换 乘"），所以只在
 * 接缝两侧都不是中日韩字符时才补空格。
 *
 * @param {string} text 原始 OCR 文本
 * @returns {string} 适合单行展示的文本
 */
export function normalizeOcrText(text) {
  if (!text) return '';
  const lines = String(text)
    .split(/\r?\n/)
    .map((line) => line.replace(/[ \t]+/g, ' ').trim())
    .filter(Boolean);

  let out = '';
  for (const line of lines) {
    if (!out) {
      out = line;
      continue;
    }
    const needsSpace = !isCjk(out[out.length - 1]) || !isCjk(line[0]);
    out += needsSpace ? ` ${line}` : line;
  }
  return out;
}

/**
 * 以第一个命中的关键词为中心截取一段上下文。
 *
 * 没有命中任何关键词时退化为取开头一段，这样纯筛选（没输关键词）的
 * 结果也有内容可看。
 *
 * @param {string} rawText 原始 OCR 文本
 * @param {string[]} tokens 查询关键词
 * @param {{ radius?: number, maxLength?: number }} [options]
 *   radius 是命中词左右各保留的字符数，maxLength 是摘要总长上限。
 * @returns {string} 处理好的摘要，首尾按需带省略号
 */
export function buildSnippet(rawText, tokens = [], options = {}) {
  const { radius = 40, maxLength = 150 } = options;
  const text = normalizeOcrText(rawText);
  if (!text) return '';

  const lowered = text.toLowerCase();
  let hitIndex = -1;
  let hitLength = 0;
  for (const token of tokens) {
    if (!token) continue;
    const index = lowered.indexOf(token.toLowerCase());
    if (index >= 0 && (hitIndex < 0 || index < hitIndex)) {
      hitIndex = index;
      hitLength = token.length;
    }
  }

  if (hitIndex < 0) {
    return text.length > maxLength ? text.slice(0, maxLength) + ELLIPSIS : text;
  }

  let start = Math.max(0, hitIndex - radius);
  let end = Math.min(text.length, hitIndex + hitLength + radius);

  // 命中词靠近开头或结尾时窗口会偏短，向另一侧补齐让摘要撑满两行。
  if (end - start < maxLength) {
    end = Math.min(text.length, start + maxLength);
    start = Math.max(0, end - maxLength);
  }

  return (start > 0 ? ELLIPSIS : '') + text.slice(start, end) + (end < text.length ? ELLIPSIS : '');
}
