import React from 'react';
import { Layout, Search as SearchIcon, Sparkles, PanelLeftOpen, PanelLeftClose } from 'lucide-react';
import { useTranslation } from 'react-i18next';

const NAV_ITEMS = [
  { id: 'preview', icon: Layout, i18nKey: 'activityBar.preview' },
  { id: 'advanced-search', icon: SearchIcon, i18nKey: 'activityBar.advancedSearch' },
  { id: 'smart-cluster', icon: Sparkles, i18nKey: 'activityBar.smartCluster', fallback: '智能聚类' },
];

export default function ActivityBar({ activeTab, setActiveTab, expanded, onToggleExpand }) {
  const { t } = useTranslation();

  return (
    <aside
      className={`hidden md:flex flex-col bg-ide-panel border-r border-ide-border h-full shrink-0 transition-all duration-200 select-none ${
        expanded ? 'w-40' : 'w-12'
      }`}
    >
      {/* Navigation icons */}
      <nav className="flex-1 flex flex-col pt-1">
        {NAV_ITEMS.map(({ id, icon: Icon, i18nKey, fallback }) => {
          const isActive = activeTab === id;
          const label = (() => {
            const translated = t(i18nKey);
            // i18next returns the key itself when missing — fall back to provided text
            return translated === i18nKey && fallback ? fallback : translated;
          })();
          return (
            <button
              key={id}
              type="button"
              data-tauri-drag-region="false"
              onClick={() => setActiveTab(id)}
              title={!expanded ? label : undefined}
              className={`relative flex items-center gap-3 h-10 cursor-pointer transition-colors overflow-hidden ${
                expanded ? 'px-3' : 'px-0 justify-center'
              } ${
                isActive
                  ? 'text-ide-text bg-ide-active'
                  : 'text-ide-muted hover:text-ide-text hover:bg-ide-hover'
              }`}
            >
              {/* Active indicator bar */}
              {isActive && (
                <span className="absolute left-0 top-1 bottom-1 w-0.5 bg-ide-accent rounded-r" />
              )}
              <Icon className="w-[18px] h-[18px] shrink-0" />
              {expanded && (
                <span className="text-sm truncate whitespace-nowrap">{label}</span>
              )}
            </button>
          );
        })}
      </nav>

      {/* Expand / Collapse toggle */}
      <div className="border-t border-ide-border">
        <button
          type="button"
          data-tauri-drag-region="false"
          onClick={onToggleExpand}
          title={expanded ? t('activityBar.collapse') : t('activityBar.expand')}
          className={`flex items-center gap-3 h-10 w-full cursor-pointer text-ide-muted hover:text-ide-text hover:bg-ide-hover transition-colors ${
            expanded ? 'px-3' : 'px-0 justify-center'
          }`}
        >
          {expanded ? (
            <>
              <PanelLeftClose className="w-[18px] h-[18px] shrink-0" />
              <span className="text-xs truncate whitespace-nowrap">{t('activityBar.collapse')}</span>
            </>
          ) : (
            <PanelLeftOpen className="w-[18px] h-[18px] shrink-0" />
          )}
        </button>
      </div>
    </aside>
  );
}
