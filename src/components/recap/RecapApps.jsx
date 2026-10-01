import React, { useState } from 'react';
import { AppWindow } from 'lucide-react';
import { useTranslation } from 'react-i18next';

const VISIBLE_APPS = 6;

function AppIcon({ name, icon }) {
  const [failedSource, setFailedSource] = useState(null);
  // Icons come from local capture metadata. Never load remote URLs here.
  const source = typeof icon === 'string' && /^(?:data:image\/(?:png|jpeg|webp|x-icon|vnd.microsoft.icon);base64,)?[a-z\d+/]+={0,2}$/i.test(icon)
    ? (icon.startsWith('data:') ? icon : `data:image/png;base64,${icon}`) : null;
  return <span role="img" aria-label={name} title={name} className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-ide-muted">
    {source && failedSource !== source
      ? <img src={source} alt="" aria-hidden="true" className="h-4 w-4 object-contain" onError={() => setFailedSource(source)} />
      : <AppWindow className="h-4 w-4" aria-hidden="true" />}
  </span>;
}

export default function RecapApps({ apps = [] }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  if (!apps.length) return null;
  const extra = apps.length - VISIBLE_APPS;
  const toggleLabel = t(expanded ? 'recap.fewerApps' : 'recap.moreApps', { count: extra });
  return <div role="group" aria-label={t('recap.apps')} className="mb-4 flex flex-wrap items-center gap-1">
    {(expanded ? apps : apps.slice(0, VISIBLE_APPS)).map((app) => <AppIcon key={app.name} {...app} />)}
    {extra > 0 && <button type="button" aria-expanded={expanded} aria-label={toggleLabel} title={toggleLabel}
      className="recap-focus h-6 rounded-md px-1.5 text-[11px] tabular-nums text-ide-muted hover:bg-ide-hover hover:text-ide-text"
      onClick={() => setExpanded(!expanded)}>{expanded ? t('recap.fewerApps') : `+${extra}`}</button>}
  </div>;
}
