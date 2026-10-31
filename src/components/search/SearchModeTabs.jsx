import React from 'react';
import { Search, Bot } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 检索模式选项卡。
 *
 * 综合搜索同时呈现文字、回顾和画面结果；AI 搜索保持独立的问答界面。
 *
 * @param {object} props
 * @param {'unified' | 'ai'} props.mode 当前模式
 * @param {(mode: 'unified' | 'ai') => void} props.onChange 切换回调
 */
export function SearchModeTabs({ mode, onChange }) {
  const { t } = useTranslation();

  const tabs = [
    { value: 'unified', label: t('advancedSearch.modes.unified'), hint: t('advancedSearch.modes.unified_hint'), Icon: Search, disabled: false },
    { value: 'ai', label: t('advancedSearch.modes.ai'), hint: t('advancedSearch.modes.ai_hint'), Icon: Bot, disabled: false },
  ];

  return (
    <div className="flex shrink-0 gap-1" role="tablist">
      {tabs.map(({ value, label, hint, Icon, disabled }) => {
        const selected = mode === value;
        return (
          <button
            key={value}
            type="button"
            role="tab"
            aria-selected={selected}
            disabled={disabled}
            title={hint}
            onClick={() => { if (!disabled) onChange(value); }}
            className={`relative flex items-center gap-1.5 rounded-t-md px-3 pb-2.5 pt-1.5 text-[13px] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:cursor-not-allowed disabled:opacity-40 ${
              selected ? 'font-semibold text-ide-accent' : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text'
            }`}
          >
            <Icon className="h-3.5 w-3.5" />
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
