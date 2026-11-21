import { describe, expect, it } from 'vitest';
import {
  buildTextSelectionLines, moveTextEndpoint, textEndpointAt,
  textSelectionContent, textWordSelection,
} from './ocr_text_selection';

const size = { width: 200, height: 100 };
const box = (id, text, x, y, width, height = 20) => ({ id, text,
  box_coords: [[x, y], [x + width, y], [x + width, y + height], [x, y + height]] });
const at = (lines, id, offset = 0) => ({ line: lines.findIndex((line) => line.id === id), offset });
function row(text, lengths, y = 5, overrides = {}) {
  return { text, box_coords: [[10, y], [110, y], [110, y + 20], [10, y + 20]],
    text_layout: { matrix: [[100, 0, 10], [0, 20, y], [0, 0, 1]], source_size: [200, 100],
      input_width: 16, valid_width: 16, time_steps: 4, centers: [1, 3, 5, 7], text_lengths: lengths,
      ...overrides } };
}

describe('screenshot text selection geometry and text', () => {
  it('keeps UTF-8 token ranges and emoji graphemes aligned under batch padding', () => {
    const [line] = buildTextSelectionLines([row('中 a🇨🇳', [3, 1, 1, 8], 5,
      { input_width: 96, valid_width: 64, time_steps: 12, centers: [2, 6, 10, 14] })], size);
    expect(line.offsets).toEqual([0, 1, 2, 3, 7]);
    expect(line.positions).toEqual([0, 0.25, 0.5, 0.75, 1]);
    expect(textEndpointAt([line], [90, 15])).toEqual({ line: 0, offset: 3 });
    expect(textEndpointAt([line], [100, 15])).toEqual({ line: 0, offset: 7 });
    const content = textSelectionContent([line], { anchor: { line: 0, offset: 1 }, focus: { line: 0, offset: 7 } });
    expect(content.text).toBe(' a🇨🇳');
    expect(content.ranges[0].quad).toEqual([[35, 5], [110, 5], [110, 25], [35, 25]]);
  });

  it('copies a reversed cross-line range in reading order with preserved newlines', () => {
    const lines = buildTextSelectionLines([row('next', [1, 1, 1, 1], 45), row('中 a🇨🇳', [3, 1, 1, 8])], size);
    const content = textSelectionContent(lines, { anchor: { line: 1, offset: 2 }, focus: { line: 0, offset: 1 } });
    expect(content.text).toBe(' a🇨🇳\nne');
    expect(content.ranges).toHaveLength(2);
    expect(content.ranges[1].quad).toEqual([[10, 45], [60, 45], [60, 65], [10, 65]]);
    expect(textEndpointAt(lines, [1000, 1000])).toEqual({ line: 1, offset: 4 });
  });

  it('hit-tests and highlights rotated text through the inverse line mapping', () => {
    const lines = buildTextSelectionLines([row('abcd', [1, 1, 1, 1], 5,
      { matrix: [[0, -20, 120], [80, 0, 5], [0, 0, 1]] })], size);
    expect(textEndpointAt(lines, [110, 31])).toEqual({ line: 0, offset: 1 });
    const content = textSelectionContent(lines, { anchor: { line: 0, offset: 1 }, focus: { line: 0, offset: 3 } });
    expect(content.text).toBe('bc');
    expect(content.ranges[0].quad).toEqual([[120, 25], [120, 65], [100, 65], [100, 25]]);
  });

  it('keeps combining marks and flags whole even when they span CTC dictionary tokens', () => {
    const lines = buildTextSelectionLines([row('a\u0301🇨🇳', [1, 2, 4, 4])], size);
    expect(lines[0].offsets).toEqual([0, 2, 6]);
    expect(moveTextEndpoint(lines, { line: 0, offset: 0 }, 1)).toEqual({ line: 0, offset: 2 });
    expect(moveTextEndpoint(lines, { line: 0, offset: 2 }, 1)).toEqual({ line: 0, offset: 6 });
  });

  it('makes legacy line boxes selectable and falls back safely from invalid token metadata', () => {
    const result = row('a🇨🇳b', [1, 1, 1, 1]);
    const lines = buildTextSelectionLines([result], size);
    expect(lines[0].estimated).toBe(true);
    expect(lines[0].offsets).toEqual([0, 1, 5, 6]);
    const legacy = buildTextSelectionLines([{ label: 'hello world', box: { x: 10, y: 5, width: 100, height: 20, unit: 'pixel' } }], size);
    const word = textWordSelection(legacy, { line: 0, offset: 2 });
    expect(textSelectionContent(legacy, word).text).toBe('hello');
  });

  it.each([1, 3])('selects centered and indented body lines without the sidebar at source scale %s', (scale) => {
    const results = [
      box('first', '正文第一行', 355, 24, 943, 18), box('menu1', '通用', 30, 51, 45, 18),
      box('wrap', '阶段', 800, 53, 36, 18), box('second', '正文第二行', 580, 79, 485, 18),
      box('menu2', '档案', 30, 91, 45, 18), box('third', '正文第三行', 493, 105, 660, 18),
      box('menu3', '玩法', 30, 130, 45, 18), box('last', '正文最后一行', 530, 131, 615, 18),
      box('distant', '另一块文字', 355, 400, 600, 18),
    ].map((result) => ({ ...result, box_coords: result.box_coords.map((point) => point.map((value) => value * scale)) }));
    const lines = buildTextSelectionLines(results, { width: 1400 * scale, height: 600 * scale });
    const anchor = at(lines, 'first', 2);
    const focus = at(lines, 'last', 4);
    const content = textSelectionContent(lines, { anchor, focus });
    expect(content.text).toBe('第一行\n阶段\n正文第二行\n正文第三行\n正文最后');
    expect(textSelectionContent(lines, { anchor: focus, focus: anchor }).text).toBe(content.text);
    expect(content.ranges.map((range) => lines[range.line].id)).toEqual(['first', 'wrap', 'second', 'third', 'last']);
    const region = lines[anchor.line].region;
    expect(textEndpointAt(lines, [400 * scale, 62 * scale], { region })).toEqual(at(lines, 'wrap'));
    expect(textEndpointAt(lines, [40 * scale, 140 * scale], { region })).toEqual(at(lines, 'last'));
    expect(textEndpointAt(lines, [40 * scale, 500 * scale], { region })).toEqual(at(lines, 'last', 6));
    expect(moveTextEndpoint(lines, at(lines, 'first', 5), 1)).toEqual(at(lines, 'wrap'));
    expect(moveTextEndpoint(lines, at(lines, 'last', 6), 1)).toEqual(at(lines, 'last', 6));
  });

  it('keeps parallel columns separate even beneath a heading spanning both columns', () => {
    const lines = buildTextSelectionLines([
      box('heading', 'Heading', 10, 5, 500),
      box('left1', 'left first', 10, 45, 200), box('right1', 'right first', 310, 48, 200),
      box('left2', 'left last', 10, 85, 200), box('right2', 'right last', 310, 88, 200),
    ], { width: 600, height: 200 });
    expect(textSelectionContent(lines, { anchor: at(lines, 'left1'), focus: at(lines, 'left2', 9) }).text)
      .toBe('left first\nleft last');
    expect(textSelectionContent(lines, { anchor: at(lines, 'left1'), focus: at(lines, 'right2', 10) }).text).toBe('');
    const region = lines[at(lines, 'left1').line].region;
    expect(textEndpointAt(lines, [400, 98], { region })).toEqual(at(lines, 'left2', 9));
  });

  it('selects rotated neighboring lines in their local reading order', () => {
    const lines = buildTextSelectionLines([
      { ...row('abcd', [1, 1, 1, 1], 5, { matrix: [[0, -20, 120], [80, 0, 5], [0, 0, 1]] }), id: 'first' },
      { ...row('next', [1, 1, 1, 1], 5, { matrix: [[0, -20, 80], [80, 0, 5], [0, 0, 1]] }), id: 'last' },
      box('menu', 'Menu', 140, 25, 40),
    ], size);
    expect(textSelectionContent(lines, { anchor: at(lines, 'first', 1), focus: at(lines, 'last', 2) }).text)
      .toBe('bcd\nne');
    expect(moveTextEndpoint(lines, at(lines, 'first', 4), 1)).toEqual(at(lines, 'last'));
  });
});
