import React from 'react';
import { ArrowLeft } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from './ui/Button';
import { workspaceLabelKey } from '../lib/workspace_navigation';

export default function WorkspaceHeader({ activeTab, returnTo, onReturn }) {
  const { t } = useTranslation();
  return (
    <header className="flex min-h-11 shrink-0 items-center gap-3 border-b border-ide-border bg-ide-panel px-4 md:px-6">
      {returnTo && activeTab === 'preview' && (
        <Button variant="ghost" size="xs" icon={ArrowLeft} onClick={onReturn} className="workspace-focus -ml-2">
          {t('workspace.backTo', { page: t(workspaceLabelKey(returnTo)) })}
        </Button>
      )}
      <h1 className="min-w-0 truncate text-sm font-semibold">{t(workspaceLabelKey(activeTab))}</h1>
    </header>
  );
}
