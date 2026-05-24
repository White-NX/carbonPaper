import React, { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Clock, Image as ImageIcon, Pencil, Trash2, Check, X,
  Flame, Snowflake, Sparkles, Pause, Play, Activity,
} from 'lucide-react';

/**
 * Shared cluster card component used by TasksView and SmartClustersView.
 *
 * Visual treatment:
 *   - Gradient background (panel → bg) for depth vs the flat outlined look
 *   - Accent strip on the left, full-height gradient using the dominant color
 *   - Top-right status orb (active / paused / pending)
 *   - Hover lift via shadow-card + 1px translate
 *   - Selected: accent ring + tinted background + subtle radial halo
 *
 * Props:
 *   variant       — 'task' | 'smart'
 *   title         — primary label
 *   subtitle      — secondary text (process / category / etc)
 *   accentColor   — hex string for the left strip and status orb tint
 *   metaChips     — array of { icon: Component, text: string, key?: string }
 *   timeRange     — optional string for the footer
 *   status        — 'active' | 'paused' | 'scoring' | 'idle' | null
 *   selected      — boolean
 *   mergeable     — show checkbox (task view merge mode)
 *   mergeChecked  — checkbox state
 *   onSelect      — () => void
 *   onToggleMerge — (id) => void
 *   onRename      — (id, newLabel) => Promise<void>
 *   onDelete      — (id) => void
 *   onTogglePause — (id) => void  (smart cluster only)
 *   id            — entity id (passed to handlers)
 *   icon          — optional override (defaults: task=Flame/Snowflake, smart=Sparkles)
 *   layerLabel    — small badge for "hot"/"cold" (task only)
 */
export default function ClusterCard({
  variant = 'task',
  id,
  title,
  subtitle,
  accentColor = '#6b7280',
  metaChips = [],
  timeRange,
  status = null,
  selected = false,
  mergeable = false,
  mergeChecked = false,
  onSelect,
  onToggleMerge,
  onRename,
  onDelete,
  onTogglePause,
  icon: IconOverride,
  layerLabel,
  ariaLabel,
}) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [draftLabel, setDraftLabel] = useState('');
  const [error, setError] = useState(null);
  const inputRef = useRef(null);

  const startEdit = (e) => {
    e.stopPropagation();
    setDraftLabel(title || '');
    setError(null);
    setEditing(true);
    setTimeout(() => inputRef.current?.focus(), 50);
  };

  const saveEdit = async (e) => {
    e.stopPropagation();
    if (draftLabel.trim() && draftLabel.trim() !== title && onRename) {
      try {
        await onRename(id, draftLabel.trim());
        setError(null);
        setEditing(false);
      } catch (err) {
        setError(err?.message || String(err));
      }
    } else {
      setEditing(false);
    }
  };

  const cancelEdit = (e) => {
    e.stopPropagation();
    setError(null);
    setEditing(false);
  };

  // Status orb: small dot at top-right.
  // active = filled accent / scoring = pulsing accent / paused = grey ring / idle = subtle
  const statusOrb = (() => {
    if (!status) return null;
    const common = 'absolute top-2 right-2 w-2 h-2 rounded-full shrink-0';
    switch (status) {
      case 'active':
        return <span className={common} style={{ backgroundColor: accentColor }} title={t('clusterCard.statusActive', 'active')} />;
      case 'scoring':
        return <span className={`${common} animate-pulse`} style={{ backgroundColor: accentColor }} title={t('clusterCard.statusScoring', 'scoring')} />;
      case 'paused':
        return <span className={`${common} border border-ide-muted/60 bg-transparent`} title={t('clusterCard.statusPaused', 'paused')} />;
      case 'idle':
      default:
        return <span className={`${common} bg-ide-muted/40`} title={t('clusterCard.statusIdle', 'idle')} />;
    }
  })();

  // Default icon based on variant
  const DefaultIcon = IconOverride
    ? IconOverride
    : variant === 'smart'
      ? Sparkles
      : layerLabel === 'cold'
        ? Snowflake
        : Flame;

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => onSelect?.(id)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect?.(id);
        }
      }}
      aria-label={ariaLabel || title}
      className={`group relative overflow-hidden cursor-pointer outline-none
                  rounded-lg border transition-all duration-150
                  focus-visible:ring-2 focus-visible:ring-ide-accent/50
                  ${selected
                    ? 'shadow-sm'
                    : 'border-ide-border hover:border-ide-accent'}`}
      style={{
        borderColor: selected ? accentColor : undefined,
        backgroundColor: selected ? `${accentColor}1C` : 'var(--ide-panel)',
      }}
    >
      {/* Left accent strip — solid color bar */}
      <div
        className="absolute left-0 top-2 bottom-2 w-1 rounded-r"
        style={{
          backgroundColor: accentColor,
        }}
        aria-hidden="true"
      />

      {/* Status orb */}
      {statusOrb}

      <div className="pl-4 pr-3 py-3 space-y-1.5">
        {/* Header row */}
        <div className="flex items-center gap-2 min-w-0">
          {mergeable && (
            <input
              type="checkbox"
              checked={mergeChecked}
              onClick={(e) => e.stopPropagation()}
              onChange={() => onToggleMerge?.(id)}
              className="w-3.5 h-3.5 rounded accent-ide-accent shrink-0"
            />
          )}

          <DefaultIcon className="w-3.5 h-3.5 shrink-0" style={{ color: accentColor }} />

          {editing ? (
            <div className="flex flex-col flex-1 min-w-0 gap-1">
              <div className="flex items-center gap-1 w-full">
                <input
                  ref={inputRef}
                  value={draftLabel}
                  onChange={(e) => { setDraftLabel(e.target.value); setError(null); }}
                  onClick={(e) => e.stopPropagation()}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') saveEdit(e);
                    if (e.key === 'Escape') cancelEdit(e);
                  }}
                  className={`flex-1 px-1.5 py-0.5 text-xs bg-ide-bg border ${error ? 'border-red-500' : 'border-ide-accent'} rounded text-ide-text focus:outline-none min-w-0`}
                />
                <button onClick={saveEdit} className="p-0.5 hover:bg-ide-hover rounded shrink-0">
                  <Check className="w-3.5 h-3.5 text-green-400" />
                </button>
                <button onClick={cancelEdit} className="p-0.5 hover:bg-ide-hover rounded shrink-0">
                  <X className="w-3.5 h-3.5 text-red-400" />
                </button>
              </div>
              {error && (
                <span className="text-[10px] text-red-400 truncate w-full" title={error}>
                  {error}
                </span>
              )}
            </div>
          ) : (
            <span className="text-sm font-semibold text-ide-text truncate flex-1">
              {title}
              {subtitle && (
                <span
                  className="ml-1.5 inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-normal align-middle"
                  style={{
                    backgroundColor: `${accentColor}22`,
                    color: accentColor,
                    border: `1px solid ${accentColor}44`,
                  }}
                >
                  {subtitle}
                </span>
              )}
            </span>
          )}

          {!editing && (
            <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
              {onTogglePause && (
                <button
                  onClick={(e) => { e.stopPropagation(); onTogglePause(id); }}
                  className="p-1 hover:bg-ide-hover rounded"
                  title={status === 'paused' ? t('clusterCard.actionResume', 'Resume') : t('clusterCard.actionPause', 'Pause')}
                >
                  {status === 'paused'
                    ? <Play className="w-3 h-3 text-ide-muted" />
                    : <Pause className="w-3 h-3 text-ide-muted" />}
                </button>
              )}
              {onRename && (
                <button
                  onClick={startEdit}
                  className="p-1 hover:bg-ide-hover rounded"
                  title={t('clusterCard.actionRename', 'Rename')}
                >
                  <Pencil className="w-3 h-3 text-ide-muted" />
                </button>
              )}
              {onDelete && (
                <button
                  onClick={(e) => { e.stopPropagation(); onDelete(id); }}
                  className="p-1 hover:bg-ide-hover rounded"
                  title={t('clusterCard.actionDelete', 'Delete')}
                >
                  <Trash2 className="w-3 h-3 text-ide-muted" />
                </button>
              )}
            </div>
          )}
        </div>

        {/* Meta chips */}
        {metaChips.length > 0 && (
          <div className="flex items-center gap-1.5 flex-wrap">
            {layerLabel && (
              <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-ide-bg border border-ide-border text-[10px] text-ide-muted font-mono uppercase">
                {layerLabel === 'cold' ? <Snowflake className="w-2.5 h-2.5" /> : <Flame className="w-2.5 h-2.5" />}
                {layerLabel}
              </span>
            )}
            {metaChips.map((chip) => {
              const Ico = chip.icon;
              return (
                <span
                  key={chip.key || chip.text}
                  className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-ide-bg border border-ide-border text-[10px] text-ide-muted font-mono"
                >
                  {Ico && <Ico className="w-2.5 h-2.5" />}
                  {chip.text}
                </span>
              );
            })}
          </div>
        )}

        {/* Time range footer */}
        {timeRange && (
          <div className="text-[10.5px] text-ide-muted/60 truncate font-mono">
            {timeRange}
          </div>
        )}
      </div>
    </div>
  );
}

// Re-export icons used by callers so they don't all need to import lucide separately
export { Clock, ImageIcon, Activity };
