import React, { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';

/**
 * 搜索头里的筛选药丸。
 *
 * @param {object} props
 * @param {React.ReactNode} props.icon 药丸左侧图标
 * @param {string} props.label 药丸文字，选中时应当带上具体值
 * @param {boolean} [props.active] 是否有生效的筛选条件
 * @param {boolean} [props.disabled] 禁用状态
 * @param {string} [props.panelClassName] 下拉面板的附加类名，用来控制宽度
 * @param {React.ReactNode | ((api: { close: () => void }) => React.ReactNode)} props.children
 *   下拉面板内容，传函数可以拿到关闭面板的方法
 */
export function FilterChip({
  icon,
  label,
  active = false,
  disabled = false,
  panelClassName = 'min-w-[220px]',
  title,
  children,
}) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef(null);

  useEffect(() => {
    if (!open) return undefined;

    const handlePointerDown = (event) => {
      if (containerRef.current && !containerRef.current.contains(event.target)) {
        setOpen(false);
      }
    };
    const handleKeyDown = (event) => {
      if (event.key === 'Escape') setOpen(false);
    };

    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  return (
    <div className="relative" ref={containerRef}>
      <button
        type="button"
        disabled={disabled}
        title={title}
        aria-expanded={open}
        aria-haspopup="true"
        onClick={() => setOpen((prev) => !prev)}
        className={`flex h-[30px] items-center gap-1.5 whitespace-nowrap rounded-full border px-3 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:cursor-not-allowed disabled:opacity-50 ${
          active
            ? 'border-ide-accent/40 bg-ide-accent/10 font-medium text-ide-accent'
            : 'border-transparent text-ide-muted hover:bg-ide-hover hover:text-ide-text'
        }`}
      >
        {icon}
        <span>{label}</span>
        <ChevronDown className={`h-3 w-3 opacity-70 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>

      {open && (
        <div
          data-testid="filter-panel"
          className={`absolute left-0 top-full z-30 mt-1.5 rounded-lg border border-ide-border bg-ide-panel p-1.5 shadow-xl ${panelClassName}`}
        >
          {typeof children === 'function' ? children({ close: () => setOpen(false) }) : children}
        </div>
      )}
    </div>
  );
}

/** 下拉面板里的小标题。 */
export function ChipPanelHeading({ children, action }) {
  return (
    <div className="mb-1 flex items-center justify-between gap-3 px-2 pb-1 pt-0.5 text-[11px] text-ide-muted">
      <span>{children}</span>
      {action}
    </div>
  );
}
