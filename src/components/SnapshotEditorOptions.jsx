import React from 'react';
import { Eraser, Grid2X2, Pencil, Type } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from './ui/Button';
import { EDITOR_COLORS, EDITOR_TOOL_LABELS } from '../lib/snapshot_editor';

const ICONS = { brush: Pencil, erase: Eraser, text: Type, mosaic: Grid2X2 };
const FONT_SIZES = [12, 14, 16, 18, 20, 24, 28, 32, 40, 48, 64];
const COLOR_LABELS = { '#e04e55': 'snapshotEditor.colors.e04e55', '#f2aa3b': 'snapshotEditor.colors.f2aa3b',
  '#3fbb7d': 'snapshotEditor.colors.3fbb7d', '#458ee8': 'snapshotEditor.colors.458ee8',
  '#ffffff': 'snapshotEditor.colors.ffffff', '#20242d': 'snapshotEditor.colors.20242d' };

export default function SnapshotEditorOptions({ editor, type }) {
  const { t } = useTranslation();
  if (!type) return null;
  const Icon = ICONS[type];
  const selected = editor.selected;
  const color = selected?.kind === 'text' ? selected.color : editor.color;
  const fontSize = selected?.kind === 'text' ? selected.fontSize : editor.fontSize;
  const sizeKey = type === 'brush' ? 'brushSize' : type === 'erase' ? 'eraseSize' : 'blocks';
  const size = selected?.kind === 'mosaic' ? selected.blocks : editor[sizeKey];
  const sizeLabel = type === 'brush' ? 'snapshotEditor.thickness' : type === 'erase' ? 'snapshotEditor.eraseRange' : 'snapshotEditor.grain';
  const fonts = [...new Set([...FONT_SIZES, fontSize])].sort((a, b) => a - b);

  return (
    <div className="snapshot-editor-options" role="group" aria-label={t('snapshotEditor.options', { tool: t(EDITOR_TOOL_LABELS[type]) })}>
      <span className="snapshot-editor-option-title"><Icon size={16} aria-hidden="true" />{t(EDITOR_TOOL_LABELS[type])}</span>
      <span className="snapshot-editor-divider" aria-hidden="true" />
      {['brush', 'text'].includes(type) && (
        <div className="snapshot-editor-swatches" role="group" aria-label={t('snapshotEditor.color')}>
          {EDITOR_COLORS.map((value) => (
            <Button key={value} variant="ghost" size="xs" className="snapshot-editor-swatch" style={{ backgroundColor: value }}
              disabled={editor.busy} aria-label={t(COLOR_LABELS[value])} aria-pressed={color === value}
              onClick={() => editor.setOption('color', value)} />
          ))}
          <input type="color" className="snapshot-editor-custom-color" value={color} disabled={editor.busy}
            aria-label={t('snapshotEditor.customColor')} title={t('snapshotEditor.customColor')}
            onPointerDown={editor.beginProperty} onFocus={editor.beginProperty}
            onChange={(event) => editor.setOption('color', event.target.value, true)} onBlur={editor.commitProperty}
            onPointerUp={editor.commitProperty} />
        </div>
      )}
      {type === 'text' ? (
        <label className="snapshot-editor-field">
          <span>{t('snapshotEditor.fontSize')}</span>
          <select value={fontSize} disabled={editor.busy} aria-label={t('snapshotEditor.fontSize')}
            onChange={(event) => editor.setOption('fontSize', Number(event.target.value))}>
            {fonts.map((value) => <option key={value} value={value}>{value} px</option>)}
          </select>
        </label>
      ) : (
        <label className="snapshot-editor-field">
          <span>{t(sizeLabel)}</span>
          <input type="range" min={type === 'brush' ? 1 : type === 'erase' ? 8 : 6}
            max={type === 'brush' ? 24 : type === 'erase' ? 80 : 48} value={size} disabled={editor.busy}
            aria-label={t(sizeLabel)} onPointerDown={editor.beginProperty}
            onChange={(event) => editor.setOption(sizeKey, Number(event.target.value), true)}
            onPointerUp={editor.commitProperty} onKeyUp={editor.commitProperty} onBlur={editor.commitProperty} />
          <output>{size} px</output>
        </label>
      )}
      {type === 'erase' && <span className="snapshot-editor-option-note">{t('snapshotEditor.eraseAnnotations')}</span>}
      {['text', 'mosaic'].includes(type) && (
        <label className="snapshot-editor-field">
          <span>{t('snapshotEditor.angle')}</span>
          <input type="number" min="-180" max="180" step="1" value={Math.round(selected?.angle || 0)}
            disabled={!selected || editor.busy} aria-label={t('snapshotEditor.angle')}
            onFocus={editor.beginProperty} onPointerDown={editor.beginProperty}
            onChange={(event) => editor.setOption('angle', Number(event.target.value), true)}
            onBlur={editor.commitProperty} onKeyUp={(event) => { if (event.key === 'Enter') editor.commitProperty(); }} />
          <span>°</span>
        </label>
      )}
    </div>
  );
}
