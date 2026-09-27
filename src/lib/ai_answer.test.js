import { describe, expect, it } from 'vitest';
import { citedIds, parseAnswer, parseInline, stripReasoning } from './ai_answer';

describe('ai answer parsing', () => {
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
