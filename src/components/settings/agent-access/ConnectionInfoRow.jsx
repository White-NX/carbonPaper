import React from 'react';
import { useTranslation } from 'react-i18next';

export default function ConnectionInfoRow({ port }) {
  const { t } = useTranslation();

  return (
    <div>
      <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.connection_info.title')}</label>
      <div className="space-y-1.5 mt-2">
        <div>
          <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.endpoint')}</p>
          <code className="text-xs text-ide-text font-mono">POST http://localhost:{port}/mcp</code>
        </div>
        <div>
          <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.auth_header')}</p>
          <code className="text-xs text-ide-text font-mono">Authorization: Bearer &lt;your-token&gt;</code>
        </div>
      </div>
    </div>
  );
}
