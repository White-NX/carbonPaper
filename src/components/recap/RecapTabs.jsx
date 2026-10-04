import React from 'react';
import { useTranslation } from 'react-i18next';
import { Sparkles, Clock } from 'lucide-react';
import { focusStyle } from '../ui/Button';

/**
 * 每日回顾顶栏选项卡（回顾 / 原始记录）。
 *
 * 采用与检索模式选项卡（SearchModeTabs）一致的设计语言：
 * 贴底指示条、统一字号与图标规范，与 PageHeader 骨架配合使用。
 *
 * @param {object} props
 * @param {'recap' | 'records'} props.tab 当前选中的选项卡
 * @param {(tab: 'recap' | 'records') => void} props.onChange 切换回调
 */
export function RecapTabs({ tab, onChange }) {
  const { t } = useTranslation();

  const tabs = [
    { value: 'recap', label: t('recap.overview'), Icon: Sparkles },
    { value: 'records', label: t('recap.records'), Icon: Clock },
  ];

  return (
    <div className="flex shrink-0 gap-1" role="tablist" aria-label={t('recap.title')}>
      {tabs.map(({ value, label, Icon }) => {
        const selected = tab === value;
        return (
          <button
            key={value}
            type="button"
            role="tab"
            id={`recap-tab-${value}`}
            aria-controls={`recap-panel-${value}`}
            aria-selected={selected}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(value)}
            onKeyDown={(event) => {
              if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
                event.preventDefault();
                const next = event.key === 'Home' ? 'recap' : event.key === 'End' ? 'records' : value === 'recap' ? 'records' : 'recap';
                onChange(next);
                document.getElementById(`recap-tab-${next}`)?.focus();
              }
            }}
            className={`relative flex items-center gap-1.5 rounded-t-md px-3 pb-2.5 pt-1.5 text-[13px] transition-colors ${focusStyle} ${
              selected
                ? 'font-semibold text-ide-accent'
                : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text'
            }`}
          >
            <Icon className="h-3.5 w-3.5" aria-hidden="true" />
            {label}
            {selected && (
              <span className="absolute inset-x-2 bottom-0 h-0.5 rounded-t-sm bg-ide-accent" />
            )}
          </button>
        );
      })}
    </div>
  );
}
