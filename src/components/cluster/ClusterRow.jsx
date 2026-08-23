import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { MoreHorizontal, Pencil, Trash2, Pause, Play, Check, X } from 'lucide-react';

/**
 * 把一条时间戳格式化成列表左列那种短标记。
 *
 * 同一天只给时分，更早的给月日，去年的给年份。侧栏只有 320px 宽，完整时间戳
 * 会挤掉标题的位置，而扫列表时真正需要的也只是「多近」这一个信息。
 */
export function formatRelativeStamp(ts) {
  if (!ts) return '';
  const date = new Date(ts.includes('T') ? ts : `${ts.replace(' ', 'T')}Z`);
  if (Number.isNaN(date.getTime())) return '';

  const now = new Date();
  const sameDay = date.getFullYear() === now.getFullYear()
    && date.getMonth() === now.getMonth()
    && date.getDate() === now.getDate();
  if (sameDay) {
    return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
  }
  if (date.getFullYear() === now.getFullYear()) {
    return `${date.getMonth() + 1}-${date.getDate()}`;
  }
  return String(date.getFullYear());
}

/**
 * 智能聚类侧栏里的一行。
 *
 * 版式沿用时间线那套：最左是固定宽度的时间列，中间是色块加名称的主行与一行
 * 来源说明，最右是快照数量。同宽的时间列和右对齐的数字让整列在垂直方向上对
 * 齐，扫的时候不需要逐行读。
 *
 * 每行只用左侧那个小色块携带颜色。早先这里还有一条按相对体量伸缩的横条，但
 * 满宽的色条在视觉上比聚类名称还重，喧宾夺主，而它表达的相对体量右侧的数字
 * 已经说清楚了。
 *
 * 已暂停的聚类整行降透明度，不再额外挂一个状态徽章：一行里能同时看到名称、
 * 来源和数量已经够密了，状态用整体的轻重来表达更省地方。
 *
 * @param {object} props
 * @param {number} props.id 聚类 id，回调时原样传回
 * @param {string} props.title 聚类名称
 * @param {string} [props.subtitle] 一行来源说明，通常是最近归档快照的出处
 * @param {string} [props.stamp] 左列时间标记，已经格式化过
 * @param {string} props.accentColor 色块与底条的颜色
 * @param {number} props.count 已归档的快照数
 * @param {number} [props.recentCount] 最近七天新增的数量，为 0 时不显示
 * @param {boolean} [props.paused] 是否已暂停
 * @param {boolean} [props.selected] 是否是当前选中项
 * @param {(id: number) => void} props.onSelect 点击整行
 * @param {(id: number, label: string) => Promise<void>} [props.onRename] 重命名
 * @param {(id: number) => void} [props.onDelete] 删除
 * @param {(id: number) => void} [props.onTogglePause] 暂停或继续
 */
export default function ClusterRow({
  id,
  title,
  subtitle,
  stamp,
  accentColor = '#6b7280',
  count = 0,
  recentCount = 0,
  paused = false,
  selected = false,
  onSelect,
  onRename,
  onDelete,
  onTogglePause,
}) {
  const { t } = useTranslation();
  const [menuOpen, setMenuOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [renameError, setRenameError] = useState(null);
  const menuRef = useRef(null);
  const inputRef = useRef(null);

  useEffect(() => {
    if (!menuOpen) return undefined;
    const handlePointerDown = (event) => {
      if (menuRef.current && !menuRef.current.contains(event.target)) setMenuOpen(false);
    };
    const handleKeyDown = (event) => {
      if (event.key === 'Escape') setMenuOpen(false);
    };
    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [menuOpen]);

  const startEdit = () => {
    setDraft(title || '');
    setRenameError(null);
    setEditing(true);
    setMenuOpen(false);
    setTimeout(() => inputRef.current?.focus(), 50);
  };

  const commitEdit = async () => {
    const next = draft.trim();
    if (!next || next === title || !onRename) {
      setEditing(false);
      return;
    }
    try {
      await onRename(id, next);
      setRenameError(null);
      setEditing(false);
    } catch (err) {
      setRenameError(err?.message || String(err));
    }
  };

  if (editing) {
    return (
      <div className="border-b border-ide-border/60 px-3 py-2.5">
        <div className="flex items-center gap-1.5">
          <input
            ref={inputRef}
            value={draft}
            onChange={(event) => { setDraft(event.target.value); setRenameError(null); }}
            onKeyDown={(event) => {
              if (event.key === 'Enter') commitEdit();
              if (event.key === 'Escape') { setRenameError(null); setEditing(false); }
            }}
            className={`min-w-0 flex-1 rounded border bg-ide-bg px-2 py-1 text-[13px] text-ide-text outline-none ${
              renameError ? 'border-red-500' : 'border-ide-accent'
            }`}
          />
          <button
            type="button"
            onClick={commitEdit}
            title={t('common.confirm', '确定')}
            className="grid h-6 w-6 shrink-0 place-items-center rounded hover:bg-ide-hover"
          >
            <Check className="h-3.5 w-3.5 text-emerald-400" />
          </button>
          <button
            type="button"
            onClick={() => { setRenameError(null); setEditing(false); }}
            title={t('common.cancel', '取消')}
            className="grid h-6 w-6 shrink-0 place-items-center rounded hover:bg-ide-hover"
          >
            <X className="h-3.5 w-3.5 text-ide-muted" />
          </button>
        </div>
        {renameError && (
          <p className="mt-1 truncate text-[10px] text-red-400" title={renameError}>{renameError}</p>
        )}
      </div>
    );
  }

  return (
    <div
      role="button"
      tabIndex={0}
      aria-label={title}
      onClick={() => onSelect?.(id)}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          onSelect?.(id);
        }
      }}
      className={`group relative cursor-pointer border-b border-ide-border/60 pl-3 pr-2.5 pb-2.5 pt-2.5 outline-none transition-colors focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ide-accent/60 ${
        selected ? 'bg-ide-accent/10' : 'hover:bg-ide-hover/40'
      } ${paused ? 'opacity-55' : ''}`}
    >
      <div className="flex items-start gap-2.5">
        <span className="w-[42px] shrink-0 pt-0.5 text-right font-mono text-[11px] tabular-nums text-ide-muted">
          {stamp}
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <span
              className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
              style={{ backgroundColor: accentColor }}
              aria-hidden="true"
            />
            <span className="truncate text-[13px] font-semibold text-ide-text" title={title}>
              {title}
            </span>
          </div>
          <p className="mt-1 truncate text-[11.5px] text-ide-muted" title={subtitle || undefined}>
            {subtitle || t('smartClusters.rowNoSource', '暂无归档快照')}
          </p>
        </div>

        <div className="shrink-0 text-right">
          <div className="font-mono text-[13px] tabular-nums text-ide-text">{count}</div>
          {recentCount > 0 && (
            <div className="mt-0.5 font-mono text-[10.5px] tabular-nums text-ide-muted">
              {t('smartClusters.rowRecent', '+{{count}} 本周', { count: recentCount })}
            </div>
          )}
        </div>
      </div>

      {(onRename || onDelete || onTogglePause) && (
        <div
          ref={menuRef}
          className="absolute right-1.5 top-1.5"
          onClick={(event) => event.stopPropagation()}
          role="presentation"
        >
          <button
            type="button"
            aria-label={t('smartClusters.rowMenu', '更多操作')}
            title={t('smartClusters.rowMenu', '更多操作')}
            aria-expanded={menuOpen}
            aria-haspopup="true"
            onClick={() => setMenuOpen((prev) => !prev)}
            className={`grid h-6 w-6 place-items-center rounded text-ide-muted transition-opacity hover:bg-ide-hover hover:text-ide-text ${
              menuOpen ? 'opacity-100' : 'opacity-0 focus-visible:opacity-100 group-hover:opacity-100'
            }`}
          >
            <MoreHorizontal className="h-3.5 w-3.5" />
          </button>

          {menuOpen && (
            <div className="absolute right-0 top-full z-30 mt-1 min-w-[130px] rounded-lg border border-ide-border bg-ide-panel p-1 shadow-xl">
              {onTogglePause && (
                <MenuItem
                  icon={paused ? <Play className="h-3.5 w-3.5" /> : <Pause className="h-3.5 w-3.5" />}
                  onClick={() => { setMenuOpen(false); onTogglePause(id); }}
                >
                  {paused ? t('clusterCard.actionResume', '继续') : t('clusterCard.actionPause', '暂停')}
                </MenuItem>
              )}
              {onRename && (
                <MenuItem icon={<Pencil className="h-3.5 w-3.5" />} onClick={startEdit}>
                  {t('clusterCard.actionRename', '重命名')}
                </MenuItem>
              )}
              {onDelete && (
                <MenuItem
                  icon={<Trash2 className="h-3.5 w-3.5" />}
                  danger
                  onClick={() => { setMenuOpen(false); onDelete(id); }}
                >
                  {t('clusterCard.actionDelete', '删除')}
                </MenuItem>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MenuItem({ icon, children, onClick, danger = false }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs transition-colors ${
        danger
          ? 'text-ide-muted hover:bg-red-500/10 hover:text-red-400'
          : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text'
      }`}
    >
      {icon}
      {children}
    </button>
  );
}
