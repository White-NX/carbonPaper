import { useLayoutEffect } from 'react';
import { useTranslation } from 'react-i18next';

// Keep this first in App's child order. Releasing the static screen's inert
// root precedes the startup dialog's layout effect and focus acquisition.
export default function StartupScreenHandoff({ ready }) {
  const { t } = useTranslation();
  useLayoutEffect(() => {
    const startup = window.__CARBONPAPER_STARTUP__;
    startup?.setLabels({
      tagline: t('startupScreen.tagline'),
      minimize: t('startupScreen.minimize'),
      hideToTray: t('topbar.hideToTray'),
    });
    if (ready) startup?.handoff();
  }, [ready, t]);
  return null;
}
