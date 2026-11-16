import { describe, expect, it } from 'vitest';
import { erasureForStroke, fitText, hitObject, resizeObject, toLocalPoint, wrapText } from './snapshot_editor';

describe('snapshot annotation geometry', () => {
  it('selects overlapping rotated objects in paint order using their rotated bounds', () => {
    const first = { id: 'first', kind: 'mosaic', cx: 100, cy: 100, w: 100, h: 20, angle: 90 };
    const top = { ...first, id: 'top', angle: 0 };
    expect(hitObject([first, top], [100, 100]).id).toBe('top');
    expect(hitObject([first, top], [100, 140]).id).toBe('first');
    expect(hitObject([first], [140, 100])).toBeNull();
    expect(toLocalPoint(first, [100, 140])[0]).toBeCloseTo(90);
  });

  it('keeps text editable and its erasure attached after rotation, scaling, and reflow', () => {
    const node = { id: 'note', kind: 'text', cx: 100, cy: 100, w: 100, h: 40, angle: 90,
      text: '测试文字', fontSize: 20, wrapWidth: 100, color: '#e04e55' };
    const mask = erasureForStroke(node, { points: [[110, 60], [110, 80]], size: 10 });
    node.erasures = [mask];
    const resized = resizeObject(node, [40, 250], 'se', 800, 600, (text, size) => [...text].length * size);
    expect(resized.text).toBe(node.text);
    expect(resized.fontSize).toBe(40);
    expect(resized.erasures).toEqual([mask]);
    expect(mask.units).toBe('em');
    expect(mask.size * resized.fontSize).toBe(20);
    expect(mask.points[0][0] * resized.fontSize + 4).toBeCloseTo(16);
  });

  it('fits mixed Chinese and emoji text without splitting characters or losing explicit blank lines', () => {
    const measure = (text) => [...text].length * 10;
    expect(wrapText('中文🙂abc\n\n末行', 30, measure)).toEqual(['中文🙂', 'abc', '', '末行']);
    const node = { text: '短句', fontSize: 20, w: 200, h: 100, wrapWidth: 200 };
    fitText(node, 800, measure);
    expect(node.w).toBe(30);
    const shortHeight = node.h;
    node.text = '第一行\n第二行';
    fitText(node, 800, measure);
    expect(node.h).toBeGreaterThan(shortHeight);
  });
});
