/**
 * Turns a model answer into plain data the AI search panel can render
 * without injecting HTML.
 *
 * Models answer in light Markdown. Only what reads well in a short answer is
 * kept: headings, bullet and numbered lists, paragraphs, bold, inline code,
 * and screenshot citations written as `[#123]`.
 */

/** Removes reasoning blocks some models stream before the answer. */
export function stripReasoning(text) {
  const withoutClosed = text.replace(/<think>[\s\S]*?<\/think>\s*/g, '');
  const open = withoutClosed.indexOf('<think>');
  return open >= 0 ? withoutClosed.slice(0, open) : withoutClosed;
}

const INLINE = /\*\*([^*]+)\*\*|`([^`]+)`|\[#(\d+)\]/g;

/** Splits a line into `{ type: 'text' | 'bold' | 'code' | 'cite', ... }` pieces. */
export function parseInline(line) {
  const parts = [];
  let last = 0;
  for (const match of line.matchAll(INLINE)) {
    if (match.index > last) parts.push({ type: 'text', text: line.slice(last, match.index) });
    if (match[1] !== undefined) parts.push({ type: 'bold', text: match[1] });
    else if (match[2] !== undefined) parts.push({ type: 'code', text: match[2] });
    else parts.push({ type: 'cite', id: Number(match[3]) });
    last = match.index + match[0].length;
  }
  if (last < line.length) parts.push({ type: 'text', text: line.slice(last) });
  return parts;
}

/** Groups lines into `heading`, `list` and `paragraph` blocks. */
export function parseAnswer(text) {
  const blocks = [];
  let paragraph = [];
  let list = null;
  const flushParagraph = () => {
    if (paragraph.length) blocks.push({ type: 'paragraph', lines: paragraph.map(parseInline) });
    paragraph = [];
  };
  const flushList = () => {
    if (list) blocks.push(list);
    list = null;
  };

  for (const raw of stripReasoning(text).split(/\r?\n/)) {
    const line = raw.trimEnd();
    const heading = line.match(/^#{1,6}\s+(.*)$/);
    const bullet = line.match(/^\s*(?:[-*•]|(\d+)[.)])\s+(.*)$/);
    if (!line.trim()) {
      flushParagraph();
      flushList();
    } else if (heading) {
      flushParagraph();
      flushList();
      blocks.push({ type: 'heading', parts: parseInline(heading[1]) });
    } else if (bullet) {
      flushParagraph();
      const ordered = bullet[1] !== undefined;
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { type: 'list', ordered, items: [] };
      }
      list.items.push(parseInline(bullet[2]));
    } else if (list && /^\s{2,}/.test(raw)) {
      // Indented continuation of the previous list item.
      list.items[list.items.length - 1].push({ type: 'text', text: ' ' }, ...parseInline(line.trim()));
    } else {
      flushList();
      paragraph.push(line);
    }
  }
  flushParagraph();
  flushList();
  return blocks;
}

/** Screenshot ids cited in the answer, in order of first appearance. */
export function citedIds(text) {
  const ids = [];
  for (const match of stripReasoning(text).matchAll(/\[#(\d+)\]/g)) {
    const id = Number(match[1]);
    if (!ids.includes(id)) ids.push(id);
  }
  return ids;
}
