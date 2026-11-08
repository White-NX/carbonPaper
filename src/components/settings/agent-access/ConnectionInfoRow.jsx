import React from 'react';
import { useTranslation } from 'react-i18next';

export default function ConnectionInfoRow({ port, mode = 'interactive' }) {
  const { t } = useTranslation();

  return (
    <div>
      <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.connection_info.title')}</label>
      <div className="space-y-1.5 mt-2">
        <div>
          <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.endpoint')}</p>
          <code className="text-xs text-ide-text font-mono">POST http://127.0.0.1:{port}/mcp</code>
        </div>
        <div>
          <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.auth_header')}</p>
          <code className="text-xs text-ide-text font-mono">Authorization: Bearer &lt;{t(mode === 'unattended' ? 'settings.security.credentials.placeholder' : 'settings.ai_embedding.token.placeholder')}&gt;</code>
        </div>
      </div>
    </div>
  );
}
