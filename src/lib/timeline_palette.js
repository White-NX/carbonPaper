/**
 * Timeline palette utilities.
 */

const PALETTE_SIZE = 8;

const PALETTE_VARS = Array.from(
  { length: PALETTE_SIZE },
  (_, index) => `--tl-app-${index + 1}`,
);

/** Fallback colors when CSS variables are unavailable. */
const FALLBACK_COLORS = [
  '#5b7fd4', '#7c6bc4', '#4a9b8e', '#c4874a',
  '#b85c6e', '#6b8cad', '#8fa05c', '#8b7b9e',
];
const FALLBACK_MUTED = '#6f7480';
const FALLBACK_ACCENT_RGB = '0 122 204';

/** Discrete opacity levels for density intensity rendering. */
const INK_STOPS = [0.10, 0.22, 0.38, 0.58, 0.82];

let cachedTheme = null;
let cachedPalette = null;

function currentTheme() {
  if (typeof document === 'undefined') return 'dark';
  return document.documentElement.classList.contains('dark') ? 'dark' : 'light';
}

/**
 * Get palette configuration for the active theme.
 */
export function getTimelinePalette() {
  const theme = currentTheme();
  if (cachedPalette && cachedTheme === theme) return cachedPalette;

  let colors = FALLBACK_COLORS;
  let muted = FALLBACK_MUTED;
  let accentRgb = FALLBACK_ACCENT_RGB;

  if (typeof document !== 'undefined' && typeof getComputedStyle === 'function') {
    const styles = getComputedStyle(document.documentElement);
    const read = (name, fallback) => styles.getPropertyValue(name).trim() || fallback;
    colors = PALETTE_VARS.map((name, index) => read(name, FALLBACK_COLORS[index]));
    muted = read('--tl-app-muted', FALLBACK_MUTED);
    accentRgb = read('--ide-accent-rgb', FALLBACK_ACCENT_RGB);
  }

  cachedPalette = { colors, muted, accentRgb };
  cachedTheme = theme;
  return cachedPalette;
}

function hashKey(key) {
  let hash = 0;
  for (let i = 0; i < key.length; i += 1) {
    hash = key.charCodeAt(i) + ((hash << 5) - hash);
  }
  return hash;
}

function paletteIndex(key) {
  return ((hashKey(key) % PALETTE_SIZE) + PALETTE_SIZE) % PALETTE_SIZE;
}

/**
 * Get palette index for a process name. Returns -1 if no process name is given.
 */
export function getProcessColorIndex(processName) {
  if (!processName) return -1;
  return paletteIndex(processName);
}

/**
 * Get color assigned to a process name.
 */
export function getProcessColor(processName) {
  const palette = getTimelinePalette();
  const index = getProcessColorIndex(processName);
  return index < 0 ? palette.muted : palette.colors[index];
}

/**
 * Map a ratio [0, 1] to a discrete opacity stop.
 */
export function inkAlpha(ratio) {
  if (!Number.isFinite(ratio) || ratio <= 0) return INK_STOPS[0];
  const clamped = Math.min(1, ratio);
  const step = Math.round(clamped * (INK_STOPS.length - 1));
  return INK_STOPS[step];
}

/**
 * Format accent color as an rgba string for canvas rendering.
 */
export function accentWithAlpha(alpha) {
  const { accentRgb } = getTimelinePalette();
  const [r, g, b] = accentRgb.split(/[\s,]+/).filter(Boolean);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * Apply opacity to a hex color string for canvas rendering.
 */
export function withAlpha(color, alpha) {
  if (typeof color !== 'string') return color;
  const hex = color.trim();
  const match = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!match) return hex;
  const value = parseInt(match[1], 16);
  const r = (value >> 16) & 255;
  const g = (value >> 8) & 255;
  const b = value & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * Generate a diagonal repeating stripe gradient for mixed session blocks.
 */
export function stripePattern(colors, alpha = 0.3, stripePx = 5) {
  const list = (colors || []).filter(Boolean);
  if (list.length === 0) return null;
  const stops = list.map(
    (color, index) => `${withAlpha(color, alpha)} ${index * stripePx}px ${(index + 1) * stripePx}px`,
  );
  return `repeating-linear-gradient(45deg, ${stops.join(', ')})`;
}

export { PALETTE_SIZE, INK_STOPS };
