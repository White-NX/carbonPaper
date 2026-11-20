import { describe, expect, it } from 'vitest';
import {
  buildTextSelectionLines, moveTextEndpoint, textEndpointAt,
  textSelectionContent, textWordSelection,
} from './ocr_text_selection';

const size = { width: 200, height: 100 };
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
});
