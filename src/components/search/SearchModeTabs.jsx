import React from 'react';
import { Type, Image as ImageIcon } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 检索模式选项卡。
 *
 * 「文字」和「画面」是同一批截图的两条平行检索通道，切换会换一批结果，
 * 所以用选项卡而不是两个并列按钮 —— 后者看起来像可以同时打开的开关。
 *
 * @param {object} props
 * @param {'ocr' | 'nl'} props.mode 当前模式
 * @param {(mode: 'ocr' | 'nl') => void} props.onChange 切换回调
 * @param {boolean} [props.backendOnline] Python 服务是否在线，离线时画面搜索不可用
 */
export function SearchModeTabs({ mode, onChange, backendOnline }) {
  const { t } = useTranslation();
  const nlDisabled = backendOnline === false;

  const tabs = [
    { value: 'ocr', label: t('advancedSearch.modes.ocr'), hint: t('advancedSearch.modes.ocr_hint'), Icon: Type, disabled: false },
    { value: 'nl', label: t('advancedSearch.modes.nl'), hint: nlDisabled ? t('search.nl.disabled_hint') : t('advancedSearch.modes.nl_hint'), Icon: ImageIcon, disabled: nlDisabled },
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
