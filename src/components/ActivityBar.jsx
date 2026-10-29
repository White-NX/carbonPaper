import React from 'react';
import { History, Search, Sparkles, CalendarDays, PanelLeftOpen, PanelLeftClose } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from './ui/Button';
import { WORKSPACE_PAGES } from '../lib/workspace_navigation';

const ICONS = { preview: History, 'advanced-search': Search, recap: CalendarDays, 'smart-cluster': Sparkles };

export default function ActivityBar({ activeTab, setActiveTab, expanded = true, onToggleExpand }) {
  const { t } = useTranslation();
  return (
    <aside className={`workspace-navigation flex h-full w-14 shrink-0 flex-col border-r border-ide-border bg-ide-panel select-none ${expanded ? 'md:w-40' : ''}`}>
      <nav aria-label={t('activityBar.navigation')} className="flex flex-1 flex-col gap-1 p-2">
        {WORKSPACE_PAGES.map(({ id, labelKey }) => {
          const Icon = ICONS[id];
          const selected = activeTab === id;
          const label = t(labelKey);
          return (
            <Button key={id} variant="ghost" icon={<Icon className="h-[18px] w-[18px] shrink-0" aria-hidden="true" />}
              aria-label={label} aria-current={selected ? 'page' : undefined} title={label}
              onClick={() => setActiveTab(id)} data-tauri-drag-region="false"
              className={`workspace-focus h-10 w-full gap-3 px-0 ${expanded ? 'md:justify-start md:px-2.5' : ''} ${selected ? 'bg-ide-accent/10 text-ide-accent' : ''}`}>
              {expanded && <span className="hidden truncate text-sm md:block">{label}</span>}
            </Button>
          );
        })}
      </nav>
      <div className="border-t border-ide-border p-2">
        <Button variant="ghost" icon={expanded ? PanelLeftClose : PanelLeftOpen} onClick={onToggleExpand}
          aria-label={t(expanded ? 'activityBar.collapse' : 'activityBar.expand')}
          title={t(expanded ? 'activityBar.collapse' : 'activityBar.expand')}
          className={`workspace-focus h-9 w-full px-0 ${expanded ? 'md:justify-start md:px-2.5' : ''}`}>
          {expanded && <span className="hidden md:block">{t('activityBar.collapse')}</span>}
        </Button>
      </div>
    </aside>
  );
}
