// Editor coordinates and sizes are always in original image pixels.
export const EDITOR_COLORS = ['#e04e55', '#f2aa3b', '#3fbb7d', '#458ee8', '#ffffff', '#20242d'];
export const EDITOR_HISTORY_LIMIT = 60;
export const EDITOR_TOOL_LABELS = { select: 'snapshotEditor.tools.select', brush: 'snapshotEditor.tools.brush',
  erase: 'snapshotEditor.tools.erase', text: 'snapshotEditor.tools.text', mosaic: 'snapshotEditor.tools.mosaic' };
export const TEXT_PADDING = 4;
export const TEXT_LEADING = 1.35;
export const TEXT_FONT = 'Segoe UI, Microsoft YaHei, sans-serif';

export const cloneScene = (scene) => structuredClone(scene);
export const scenesEqual = (first, second) => JSON.stringify(first) === JSON.stringify(second);
export const clamp = (value, min, max) => Math.max(min, Math.min(max, value));

export function toLocalPoint(node, point) {
  const angle = -(node.angle || 0) * Math.PI / 180;
  const x = point[0] - node.cx;
  const y = point[1] - node.cy;
  return [x * Math.cos(angle) - y * Math.sin(angle) + node.w / 2,
    x * Math.sin(angle) + y * Math.cos(angle) + node.h / 2];
}

export function objectBounds(node) {
  const angle = (node.angle || 0) * Math.PI / 180;
  return {
    hx: (Math.abs(Math.cos(angle)) * node.w + Math.abs(Math.sin(angle)) * node.h) / 2,
    hy: (Math.abs(Math.sin(angle)) * node.w + Math.abs(Math.cos(angle)) * node.h) / 2,
  };
}

export function constrainObject(node, width, height) {
  const bounds = objectBounds(node);
  node.cx = bounds.hx * 2 > width ? width / 2 : clamp(node.cx, bounds.hx, width - bounds.hx);
  node.cy = bounds.hy * 2 > height ? height / 2 : clamp(node.cy, bounds.hy, height - bounds.hy);
  return node;
}

export function hitObject(scene, point) {
  return [...scene].reverse().find((node) => {
    if (!['text', 'mosaic'].includes(node.kind)) return false;
    const [x, y] = toLocalPoint(node, point);
    return x >= 0 && y >= 0 && x <= node.w && y <= node.h;
  }) || null;
}

export function wrapText(text, maxWidth, measure) {
  const rows = [];
  for (const paragraph of text.split('\n')) {
    let line = '';
    for (const character of paragraph) {
      if (line && measure(line + character) > maxWidth) {
        rows.push(line);
        line = character;
      } else line += character;
    }
    rows.push(line);
  }
  return rows;
}

export function fitText(node, imageWidth, measure, placeholder = '') {
  const limit = Math.max(node.fontSize,
    Math.min(node.wrapWidth || imageWidth - 16, imageWidth - 16) - TEXT_PADDING * 2 - 2);
  const rows = wrapText(node.text || placeholder, limit, measure);
  node.w = Math.ceil(Math.max(node.fontSize, ...rows.map(measure)) + TEXT_PADDING * 2 + 2);
  node.h = Math.ceil(rows.length * node.fontSize * TEXT_LEADING + TEXT_PADDING * 2);
  return node;
}

export function resizeObject(original, point, corner, width, height, measure) {
  const node = structuredClone(original);
  const [x, y] = toLocalPoint(original, point);
  const oppositeX = corner.includes('w') ? original.w : 0;
  const oppositeY = corner.includes('n') ? original.h : 0;
  const minWidth = node.kind === 'text' ? 12 + TEXT_PADDING * 2 : 12;
  const minHeight = node.kind === 'text' ? 12 * TEXT_LEADING + TEXT_PADDING * 2 : 12;
  const factor = Math.max(minWidth / original.w, minHeight / original.h,
    Math.abs(x - oppositeX) / original.w, Math.abs(y - oppositeY) / original.h);
  const bounded = Math.min(factor, width / original.w, height / original.h);
  node.w = original.w * bounded;
  node.h = original.h * bounded;
  if (node.kind === 'text') {
    node.fontSize = clamp(Math.round(original.fontSize * bounded), 12, 256);
    if (node.wrapWidth) node.wrapWidth = original.wrapWidth * bounded;
    fitText(node, width, (text) => measure(text, node.fontSize));
  }
  return constrainObject(node, width, height);
}

export function strokePath(context, points, size, color = '#000000') {
  if (!points.length) return;
  context.lineWidth = size;
  context.lineCap = 'round';
  context.lineJoin = 'round';
  context.strokeStyle = color;
  context.fillStyle = color;
  context.beginPath();
  points.forEach(([x, y], index) => index ? context.lineTo(x, y) : context.moveTo(x, y));
  if (points.length === 1) {
    context.arc(points[0][0], points[0][1], size / 2, 0, Math.PI * 2);
    context.fill();
  } else context.stroke();
}

function brushBox(node) {
  const xs = node.points.map((point) => point[0]);
  const ys = node.points.map((point) => point[1]);
  const left = Math.floor(Math.min(...xs) - node.size / 2 - 1);
  const top = Math.floor(Math.min(...ys) - node.size / 2 - 1);
  const w = Math.ceil(Math.max(...xs) + node.size / 2 + 1) - left;
  const h = Math.ceil(Math.max(...ys) + node.size / 2 + 1) - top;
  return { cx: left + w / 2, cy: top + h / 2, w, h, angle: 0 };
}

export function erasureForStroke(node, stroke) {
  const box = node.kind === 'brush' ? brushBox(node) : node;
  const points = stroke.points.map((point) => toLocalPoint(box, point));
  if (node.kind === 'text') {
    return { units: 'em', points: points.map((point) => point.map((v) => (v - TEXT_PADDING) / node.fontSize)),
      size: stroke.size / node.fontSize,
      clip: [-TEXT_PADDING / node.fontSize, -TEXT_PADDING / node.fontSize, box.w / node.fontSize, box.h / node.fontSize] };
  }
  return { units: 'pixel', points, size: stroke.size, width: box.w, height: box.h, clip: [0, 0, box.w, box.h] };
}

export function applyErasure(context, node, mask, box = node) {
  context.save();
  context.globalCompositeOperation = 'destination-out';
  if (mask.units === 'em') {
    context.translate(TEXT_PADDING, TEXT_PADDING);
    context.scale(node.fontSize, node.fontSize);
  } else context.scale(box.w / mask.width, box.h / mask.height);
  context.beginPath();
  context.rect(...mask.clip);
  context.clip();
  strokePath(context, mask.points, mask.size);
  context.restore();
}

function createCanvas(width, height) {
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.ceil(width));
  canvas.height = Math.max(1, Math.ceil(height));
  return canvas;
}

function paintObject(node, merged, scratch) {
  const box = node.kind === 'brush' ? brushBox(node) : node;
  scratch.width = Math.max(1, Math.ceil(box.w));
  scratch.height = Math.max(1, Math.ceil(box.h));
  const context = scratch.getContext('2d', { willReadFrequently: true });
  if (node.kind === 'brush') {
    strokePath(context, node.points.map((point) => toLocalPoint(box, point)), node.size, node.color);
  } else if (node.kind === 'text') {
    context.font = '500 ' + node.fontSize + 'px ' + TEXT_FONT;
    context.fillStyle = node.color;
    context.textBaseline = 'top';
    const lines = wrapText(node.text, Math.max(node.fontSize, box.w - TEXT_PADDING * 2 - 2),
      (text) => context.measureText(text).width);
    lines.forEach((line, index) => context.fillText(line, TEXT_PADDING, TEXT_PADDING + index * node.fontSize * TEXT_LEADING));
  } else if (node.kind === 'mosaic') {
    // Sample in the object's coordinates so rotation and resizing sample the
    // underlying image again rather than moving a cached patch of private pixels.
    const cut = createCanvas(box.w, box.h);
    const cutContext = cut.getContext('2d');
    cutContext.translate(box.w / 2, box.h / 2);
    cutContext.rotate(-(node.angle || 0) * Math.PI / 180);
    cutContext.drawImage(merged, -node.cx, -node.cy);
    const small = createCanvas(Math.ceil(box.w / node.blocks), Math.ceil(box.h / node.blocks));
    small.getContext('2d').drawImage(cut, 0, 0, small.width, small.height);
    context.imageSmoothingEnabled = false;
    context.drawImage(small, 0, 0, box.w, box.h);
  }
  for (const mask of node.erasures || []) applyErasure(context, node, mask, box);
  return { box, context };
}

function compositeObject(mergedContext, scratch, box) {
  mergedContext.save();
  mergedContext.translate(box.cx, box.cy);
  mergedContext.rotate((box.angle || 0) * Math.PI / 180);
  mergedContext.drawImage(scratch, -box.w / 2, -box.h / 2, box.w, box.h);
  mergedContext.restore();
}

export function renderScene(canvas, image, scene, { editingId = null } = {}) {
  const width = image.naturalWidth || image.width;
  const height = image.naturalHeight || image.height;
  if (canvas.width !== width) canvas.width = width;
  if (canvas.height !== height) canvas.height = height;
  const context = canvas.getContext('2d');
  context.clearRect(0, 0, width, height);
  context.drawImage(image, 0, 0, width, height);
  const scratch = createCanvas(1, 1);
  for (const node of scene) {
    if (node.id === editingId) continue;
    const { box } = paintObject(node, canvas, scratch);
    compositeObject(context, scratch, box);
  }
}

export function eraseScene(image, scene, stroke) {
  const result = cloneScene(scene);
  const merged = createCanvas(image.naturalWidth || image.width, image.naturalHeight || image.height);
  const mergedContext = merged.getContext('2d');
  mergedContext.drawImage(image, 0, 0);
  const scratch = createCanvas(1, 1);
  for (const node of result) {
    const { box, context } = paintObject(node, merged, scratch);
    const mask = erasureForStroke(node, stroke);
    const localPoints = stroke.points.map((point) => toLocalPoint(box, point));
    const left = clamp(Math.floor(Math.min(...localPoints.map((p) => p[0])) - stroke.size), 0, scratch.width);
    const top = clamp(Math.floor(Math.min(...localPoints.map((p) => p[1])) - stroke.size), 0, scratch.height);
    const right = clamp(Math.ceil(Math.max(...localPoints.map((p) => p[0])) + stroke.size), 0, scratch.width);
    const bottom = clamp(Math.ceil(Math.max(...localPoints.map((p) => p[1])) + stroke.size), 0, scratch.height);
    if (right > left && bottom > top) {
      const before = context.getImageData(left, top, right - left, bottom - top).data;
      applyErasure(context, node, mask, box);
      const after = context.getImageData(left, top, right - left, bottom - top).data;
      for (let index = 3; index < before.length; index += 4) {
        if (after[index] < before[index]) {
          node.erasures = [...(node.erasures || []), mask];
          break;
        }
      }
    }
    compositeObject(mergedContext, scratch, box);
  }
  return result;
}

export function textErasureMask(node) {
  if (!node.erasures?.length) return undefined;
  const canvas = createCanvas(node.w, node.h);
  const context = canvas.getContext('2d');
  context.fillStyle = '#ffffff';
  context.fillRect(0, 0, canvas.width, canvas.height);
  for (const mask of node.erasures) applyErasure(context, node, mask);
  return 'url(' + canvas.toDataURL() + ')';
}
