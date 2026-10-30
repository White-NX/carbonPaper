import React from 'react';
import { cn } from '../lib/utils';

/**
 * 主区域各页面共用的紧凑工具区。
 * 主要动作、选项卡和结果统计共用第一行；筛选或状态行只在有内容时占据高度。
 *
 * 需要提交行为的页面（比如检索页的搜索框）可以传 `as="form"`，其余属性会
 * 透传给最外层元素。
 *
 * @param {object} props
 * @param {'div' | 'form'} [props.as] 渲染成什么元素，默认 div
 * @param {React.ReactNode} props.children 第一行内容
 * @param {string} [props.primaryRowClassName] 第一行的附加类名
 * @param {React.ReactNode} [props.secondaryRow] 第二行内容
 * @param {string} [props.secondaryRowClassName] 第二行的附加类名
 * @param {boolean} [props.bordered] 是否画出底部分隔线。下面紧跟着统计条时
 *   应当传 false，由统计条自己的上边界承担分隔
 * @param {boolean} [props.flushBottom] 第二行是否贴着顶栏下沿。选项卡那种
 *   自带下划线的控件需要贴住，药丸则需要下方留白
 * @param {string} [props.className] 附加类名
 */
export function PageHeader({
  as: Tag = 'div',
  children,
  primaryRowClassName,
  secondaryRow,
  secondaryRowClassName,
  bordered = true,
  flushBottom = false,
  className = '',
  ...rest
}) {
  return (
    <Tag
      className={`shrink-0 bg-ide-panel px-4 pt-2 ${flushBottom ? '' : 'pb-2'} ${
        bordered ? 'border-b border-ide-border' : ''
      } ${className}`}
      {...rest}
    >
      <div className={cn('flex min-h-9 items-center gap-2', primaryRowClassName)}>{children}</div>
      {secondaryRow !== undefined && (
        <div className={cn('mt-2 flex min-h-8 items-center gap-3 empty:hidden', secondaryRowClassName)}>{secondaryRow}</div>
      )}
    </Tag>
  );
}

/**
 * 顶栏第二行里的状态药丸。
 *
 * 和检索页筛选用的 `FilterChip` 共用同一套外形，区别是它不打开下拉面板：
 * 这里的药丸报告的是后台状态，本身不可点，只在带 `action` 时右端多一个
 * 小按钮（例如待处理数量后面跟着的「现在整理」）。
 *
 * @param {object} props
 * @param {React.ReactNode} [props.icon] 左侧图标
 * @param {React.ReactNode} props.children 药丸文字
 * @param {'neutral' | 'accent' | 'warning'} [props.tone] 配色
 * @param {React.ReactNode} [props.action] 右端附带的按钮
 * @param {string} [props.title] 悬停说明
 */
export function StatusChip({ icon, children, tone = 'neutral', action, title }) {
  const toneClass = {
    neutral: 'border-ide-border/70 text-ide-muted',
    accent: 'border-ide-accent/40 bg-ide-accent/10 text-ide-accent',
    warning: 'border-amber-500/40 bg-amber-500/10 text-amber-400',
  }[tone];

  return (
    <span
      title={title}
      className={`flex h-[30px] items-center gap-1.5 whitespace-nowrap rounded-full border pl-3 text-xs ${action ? 'pr-1' : 'pr-3'} ${toneClass}`}
    >
      {icon}
      <span>{children}</span>
      {action}
    </span>
  );
}
