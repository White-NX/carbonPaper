import { describe, expect, it } from 'vitest';
import { citedIds, parseAnswer, parseInline, stripReasoning } from './ai_answer';

describe('ai answer parsing', () => {
  it('parses tables between prose with alignment and inline formatting', () => {
    const blocks = parseAnswer('Results\n| Paper | Year | Source |\n| :--- | ---: | :---: |\n| **RoleLLM** | 2023 | [#42] |\n\nAfter');
    expect(blocks.map((block) => block.type)).toEqual(['paragraph', 'table', 'paragraph']);
    expect(blocks[1]).toMatchObject({
      align: ['left', 'right', 'center'],
      rows: [[[{ type: 'bold', text: 'RoleLLM' }], [{ type: 'text', text: '2023' }], [{ type: 'cite', id: 42 }]]],
    });
  });

  it('handles optional outer pipes, escaped pipes and uneven rows', () => {
    const [table] = parseAnswer('Name | Value\n--- | ---\n`a\\|b` | x\\|y\nshort |\nextra | value | ignored');
    expect(table.header).toHaveLength(2);
    expect(table.rows).toEqual([
      [[{ type: 'code', text: 'a|b' }], [{ type: 'text', text: 'x|y' }]],
      [[{ type: 'text', text: 'short' }], []],
      [[{ type: 'text', text: 'extra' }], [{ type: 'text', text: 'value' }]],
    ]);
  });

  it('keeps incomplete or invalid table headers as prose while streaming', () => {
    for (const text of ['a | b', 'a | b\n--- |', 'a | b\n--- | --- | ---', 'a | b\ntext | text']) {
      expect(parseAnswer(text).every((block) => block.type === 'paragraph')).toBe(true);
    }
    expect(parseAnswer('a | b\n--- | ---')[0]).toMatchObject({ type: 'table', rows: [] });
  });

  it('drops reasoning blocks, including one still streaming', () => {
    expect(stripReasoning('<think>plan</think>\nAnswer')).toBe('Answer');
    expect(stripReasoning('Answer<think>still thinking')).toBe('Answer');
  });

  it('splits inline bold, code and citations', () => {
    expect(parseInline('See **this** in `a.js` [#12].')).toEqual([
      { type: 'text', text: 'See ' },
      { type: 'bold', text: 'this' },
      { type: 'text', text: ' in ' },
      { type: 'code', text: 'a.js' },
      { type: 'text', text: ' ' },
      { type: 'cite', id: 12 },
      { type: 'text', text: '.' },
    ]);
  });

  it('groups headings, lists and paragraphs', () => {
    const blocks = parseAnswer('## Result\nFirst line\nsecond line\n\n- one [#1]\n- two\n1. first\n2. second');
    expect(blocks.map((b) => b.type)).toEqual(['heading', 'paragraph', 'list', 'list']);
    expect(blocks[1].lines).toHaveLength(2);
    expect(blocks[2]).toMatchObject({ ordered: false });
    expect(blocks[2].items).toHaveLength(2);
    expect(blocks[3]).toMatchObject({ ordered: true });
  });

  it('lists cited ids once, in order', () => {
    expect(citedIds('a [#5] b [#2] c [#5]')).toEqual([5, 2]);
  });
});
