import React from 'react';

/**
 * 主区域各页面共用的顶栏骨架。
 *
 * 检索页、智能聚类页和创建流程原本各自写了一套顶栏，行高、内边距和分隔线
 * 都不一样，放在相邻标签页里切换时能明显看出断层。这里把三者的度量统一到
 * 一处：外层的留白、两行之间的间距、第二行的最小高度都由这个组件决定，
 * 页面只负责往两行里放东西。
 *
 * 第一行放当前页面的主要动作（检索页是搜索框，聚类页是「新建」按钮）；
 * 第二行放选项卡、筛选药丸或状态药丸。第二行留空时不会塌陷，因为切换页面
 * 时顶栏高度跳动比空一行更扎眼。
 *
 * 需要提交行为的页面（比如检索页的搜索框）可以传 `as="form"`，其余属性会
 * 透传给最外层元素。
 *
 * @param {object} props
 * @param {'div' | 'form'} [props.as] 渲染成什么元素，默认 div
 * @param {React.ReactNode} props.children 第一行内容
 * @param {React.ReactNode} [props.secondaryRow] 第二行内容
 * @param {boolean} [props.bordered] 是否画出底部分隔线。下面紧跟着统计条时
 *   应当传 false，由统计条自己的上边界承担分隔
 * @param {boolean} [props.flushBottom] 第二行是否贴着顶栏下沿。选项卡那种
 *   自带下划线的控件需要贴住，药丸则需要下方留白
 * @param {string} [props.className] 附加类名
 */
export function PageHeader({
  as: Tag = 'div',
  children,
  secondaryRow,
  bordered = true,
  flushBottom = false,
  className = '',
  ...rest
}) {
  return (
    <Tag
      className={`shrink-0 bg-ide-panel px-6 pt-4 ${flushBottom ? '' : 'pb-3.5'} ${
        bordered ? 'border-b border-ide-border' : ''
      } ${className}`}
      {...rest}
    >
      <div className="flex min-h-10 items-center gap-3">{children}</div>
      {secondaryRow !== undefined && (
        <div className="mt-3.5 flex min-h-[34px] items-center gap-7">{secondaryRow}</div>
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
