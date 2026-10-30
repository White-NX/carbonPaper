import React from 'react';
import { useTranslation } from 'react-i18next';
import { workspaceLabelKey } from '../lib/workspace_navigation';

export default function WorkspaceHeader({ activeTab }) {
  const { t } = useTranslation();
  return (
    <h1 className="sr-only">{t(workspaceLabelKey(activeTab))}</h1>
  );
}
