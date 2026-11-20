// Selection stays one-dimensional within each rectified OCR line. Quads are
// generated only for visible lines and the selected range, never per glyph.
const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
const characterSegmenter = typeof Intl.Segmenter === 'function'
  ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
const wordSegmenter = typeof Intl.Segmenter === 'function'
  ? new Intl.Segmenter(undefined, { granularity: 'word' }) : null;

function characters(text) {
  if (characterSegmenter) return Array.from(characterSegmenter.segment(text));
  let index = 0;
  return Array.from(text, (segment) => {
    const result = { segment, index };
    index += segment.length;
    return result;
  });
}

export function projectTextPoint(matrix, point) {
  const [x, y] = point;
  const divisor = matrix[2][0] * x + matrix[2][1] * y + matrix[2][2];
  if (Math.abs(divisor) < 1e-12) return null;
  const result = [
    (matrix[0][0] * x + matrix[0][1] * y + matrix[0][2]) / divisor,
    (matrix[1][0] * x + matrix[1][1] * y + matrix[1][2]) / divisor,
  ];
  return result.every(Number.isFinite) ? result : null;
}

function inverse(matrix) {
  const [[a, b, c], [d, e, f], [g, h, i]] = matrix;
  const determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
  if (!Number.isFinite(determinant) || Math.abs(determinant) < 1e-12) return null;
  return [[e * i - f * h, c * h - b * i, b * f - c * e],
    [f * g - d * i, a * i - c * g, c * d - a * f],
    [d * h - e * g, b * g - a * h, a * e - b * d]]
    .map((row) => row.map((value) => value / determinant));
}

function quadMatrix(points) {
  const [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = points;
  const sx = x0 - x1 + x2 - x3;
  const sy = y0 - y1 + y2 - y3;
  const dx1 = x1 - x2;
  const dx2 = x3 - x2;
  const dy1 = y1 - y2;
  const dy2 = y3 - y2;
  let g = 0;
  let h = 0;
  if (Math.abs(sx) + Math.abs(sy) > 1e-8) {
    const denominator = dx1 * dy2 - dx2 * dy1;
    if (Math.abs(denominator) < 1e-12) return null;
    g = (sx * dy2 - dx2 * sy) / denominator;
    h = (dx1 * sy - sx * dy1) / denominator;
  }
  return [[x1 - x0 + g * x1, x3 - x0 + h * x3, x0],
    [y1 - y0 + g * y1, y3 - y0 + h * y3, y0], [g, h, 1]];
}

function characterWeight(text) {
  if (/\s/u.test(text)) return 0.6;
  if (/[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}\p{Extended_Pictographic}\p{Regional_Indicator}]/u.test(text)) return 2;
  if (/^[ilI.,:;'!|]$/.test(text)) return 0.5;
  if (/^[MWmw]$/.test(text)) return 1.4;
  return 1;
}

function appendCharacters(cells, text, offset, from, to) {
  const parts = characters(text);
  const weights = parts.map((part) => characterWeight(part.segment));
  const total = weights.reduce((sum, weight) => sum + weight, 0);
  let position = from;
  parts.forEach((part, index) => {
    const end = index === parts.length - 1 ? to : position + (to - from) * weights[index] / total;
    cells.push({ start: offset + part.index, end: offset + part.index + part.segment.length,
      from: position, to: end });
    position = end;
  });
}

function alignedCells(text, layout) {
  if (!layout || !Array.isArray(layout.centers) || !Array.isArray(layout.text_lengths)
    || !layout.centers.length || layout.centers.length !== layout.text_lengths.length
    || !Number.isInteger(layout.time_steps) || layout.time_steps <= 0
    || !Number.isInteger(layout.input_width) || !Number.isInteger(layout.valid_width)
    || layout.valid_width <= 0 || layout.input_width < layout.valid_width) return null;
  const scale = Math.fround(Math.fround(layout.input_width / layout.time_steps) / layout.valid_width);
  const centers = layout.centers.map((value) => Math.fround(clamp(Math.fround(value * 0.5 * scale), 0, 1)));
  if (layout.centers.some((value, index) => !Number.isInteger(value) || value <= 0
    || value > layout.time_steps * 2 || (index > 0 && value <= layout.centers[index - 1]))) return null;
  const bytes = new TextEncoder().encode(text);
  const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
  const cells = [];
  let byteOffset = 0;
  let textOffset = 0;
  let position = 0;
  try {
    layout.text_lengths.forEach((length, index) => {
      if (!Number.isInteger(length) || length <= 0 || byteOffset + length > bytes.length) throw new Error('range');
      const token = decoder.decode(bytes.subarray(byteOffset, byteOffset + length));
      const end = index === centers.length - 1 ? 1
        : Math.fround(Math.fround(centers[index] + centers[index + 1]) * 0.5);
      appendCharacters(cells, token, textOffset, position, end);
      byteOffset += length;
      textOffset += token.length;
      position = end;
    });
  } catch { return null; }
  if (byteOffset !== bytes.length || textOffset !== text.length) return null;
  // Combining marks and emoji sequences can span dictionary tokens. Carets
  // still follow whole-text grapheme boundaries, as native browser selection does.
  let cellIndex = 0;
  return characters(text).map((part) => {
    while (cells[cellIndex].end <= part.index) cellIndex += 1;
    const from = cells[cellIndex].from;
    const end = part.index + part.segment.length;
    while (cells[cellIndex].end < end) cellIndex += 1;
    return { start: part.index, end, from, to: cells[cellIndex].to };
  });
}

export function textRangeQuad(line, from, to) {
  const points = [[from, 0], [to, 0], [to, 1], [from, 1]]
    .map((point) => projectTextPoint(line.matrix, point));
  if (points.some((point) => !point)) return null;
  return points.map(([x, y]) => [clamp(x, 0, line.sourceSize[0] - 1), clamp(y, 0, line.sourceSize[1] - 1)]);
}

export function buildTextSelectionLines(results = [], naturalSize) {
  if (!naturalSize?.width || !naturalSize?.height) return [];
  const lines = [];
  results.forEach((result, index) => {
    const text = result.text || result.label || '';
    if (!text) return;
    let layout = result.text_layout;
    let cells = alignedCells(text, layout);
    let matrix = cells && layout.matrix;
    if (!Array.isArray(matrix) || matrix.length !== 3 || matrix.some((row) => !Array.isArray(row)
      || row.length !== 3 || row.some((value) => !Number.isFinite(value))) || !inverse(matrix)) {
      cells = null;
    }
    if (!cells) {
      layout = null;
      let points = result.box_coords || result.points;
      if (!points && result.box) {
        const { x, y, width, height, unit } = result.box;
        const sx = unit === 'pixel' ? 1 : naturalSize.width;
        const sy = unit === 'pixel' ? 1 : naturalSize.height;
        points = [[x * sx, y * sy], [(x + width) * sx, y * sy],
          [(x + width) * sx, (y + height) * sy], [x * sx, (y + height) * sy]];
      }
      if (!Array.isArray(points) || points.length !== 4 || points.some((point) => !Array.isArray(point)
        || point.length !== 2 || point.some((value) => !Number.isFinite(value)))) return;
      const horizontal = Math.hypot(points[1][0] - points[0][0], points[1][1] - points[0][1]);
      const vertical = Math.hypot(points[3][0] - points[0][0], points[3][1] - points[0][1]);
      if (vertical > horizontal * 1.5) points = [points[1], points[2], points[3], points[0]];
      matrix = quadMatrix(points);
      cells = [];
      appendCharacters(cells, text, 0, 0, 1);
    }
    const inverted = matrix && inverse(matrix);
    if (!inverted || !cells.length) return;
    const sourceSize = layout?.source_size || [naturalSize.width, naturalSize.height];
    if (!Array.isArray(sourceSize) || sourceSize.length !== 2
      || sourceSize.some((value) => !Number.isInteger(value) || value <= 0)) return;
    const line = { id: String(result.id ?? index), text, matrix, inverse: inverted, cells, sourceSize,
      estimated: !layout, offsets: [0, ...cells.map((cell) => cell.end)],
      positions: [0, ...cells.map((cell) => cell.to)] };
    line.quad = textRangeQuad(line, 0, 1);
    if (!line.quad) return;
    line.top = Math.min(...line.quad.map((point) => point[1]));
    line.left = Math.min(...line.quad.map((point) => point[0]));
    lines.push(line);
  });
  return lines.sort((a, b) => a.top - b.top || a.left - b.left);
}

function polygonDistance(point, polygon) {
  let inside = false;
  let distance = Infinity;
  const [x, y] = point;
  polygon.forEach(([ax, ay], index) => {
    const [bx, by] = polygon[(index + 1) % polygon.length];
    if ((ay > y) !== (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax) inside = !inside;
    const dx = bx - ax;
    const dy = by - ay;
    const t = clamp(((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy || 1), 0, 1);
    distance = Math.min(distance, (x - ax - t * dx) ** 2 + (y - ay - t * dy) ** 2);
  });
  return inside ? 0 : distance;
}

function endpointOnLine(lines, lineIndex, point, character) {
  const line = lines[lineIndex];
  const normalized = projectTextPoint(line.inverse, point);
  if (!normalized) return null;
  const u = clamp(normalized[0], 0, 1);
  let low = 0;
  let high = line.positions.length - 1;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (line.positions[mid] < u || (character && line.positions[mid] === u)) low = mid + 1;
    else high = mid;
  }
  if (low > 0 && (character || u - line.positions[low - 1] <= line.positions[low] - u)) low -= 1;
  return { line: lineIndex, offset: line.offsets[low] };
}

export function textEndpointAt(lines, point, { lineIndex, preferredLine, character = false } = {}) {
  if (Number.isInteger(lineIndex) && lines[lineIndex]) return endpointOnLine(lines, lineIndex, point, character);
  let best = -1;
  let distance = Infinity;
  lines.forEach((line, index) => {
    const candidate = polygonDistance(point, line.quad);
    if (candidate < distance || (candidate === distance && index === preferredLine)) {
      distance = candidate;
      best = index;
    }
  });
  return best >= 0 ? endpointOnLine(lines, best, point, character) : null;
}

export function textSelectionContent(lines, selection) {
  if (!selection?.anchor || !selection.focus) return { text: '', ranges: [] };
  let { anchor: start, focus: end } = selection;
  if (start.line > end.line || (start.line === end.line && start.offset > end.offset)) [start, end] = [end, start];
  if (!lines[start.line] || !lines[end.line]) return { text: '', ranges: [] };
  const texts = [];
  const ranges = [];
  for (let index = start.line; index <= end.line; index += 1) {
    const line = lines[index];
    const from = index === start.line ? start.offset : 0;
    const to = index === end.line ? end.offset : line.text.length;
    texts.push(line.text.slice(from, to));
    if (from === to) continue;
    const startIndex = line.offsets.indexOf(from);
    const endIndex = line.offsets.indexOf(to);
    if (startIndex < 0 || endIndex < 0) continue;
    const quad = textRangeQuad(line, line.positions[startIndex], line.positions[endIndex]);
    if (quad) ranges.push({ line: index, quad, estimated: line.estimated });
  }
  return { text: texts.join('\n'), ranges };
}

export function textWordSelection(lines, endpoint) {
  const line = lines[endpoint.line];
  const position = Math.min(endpoint.offset, line.text.length - 1);
  const words = wordSegmenter ? Array.from(wordSegmenter.segment(line.text))
    : Array.from(line.text.matchAll(/\S+|\s+/gu), (match) => ({ segment: match[0], index: match.index }));
  const word = words.find((part) => part.index <= position && part.index + part.segment.length > position);
  return word ? { anchor: { line: endpoint.line, offset: word.index },
    focus: { line: endpoint.line, offset: word.index + word.segment.length } } : null;
}

export function moveTextEndpoint(lines, endpoint, direction) {
  const line = lines[endpoint.line];
  const current = line.offsets.indexOf(endpoint.offset);
  const next = current + direction;
  if (next >= 0 && next < line.offsets.length) return { line: endpoint.line, offset: line.offsets[next] };
  const lineIndex = endpoint.line + direction;
  if (!lines[lineIndex]) return endpoint;
  return { line: lineIndex, offset: direction > 0 ? 0 : lines[lineIndex].text.length };
}
