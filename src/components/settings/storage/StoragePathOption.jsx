import React from 'react';
import { useTranslation } from 'react-i18next';

export default function StoragePathOption({
  label,
  value,
  onChangePath,
  icon: Icon,
  description,
  error,
  disabled,
  className = '',
}) {
  const { t } = useTranslation();

  return (
    <div className={`bg-ide-bg/70 border border-ide-border rounded-xl p-4 ${className}`}>
      <div className="flex items-center gap-3 mb-3">
        {Icon && (
          <div className="p-2 rounded-lg bg-ide-panel border border-ide-border">
            <Icon className="w-4 h-4" />
          </div>
        )}
        <div className="flex-1">
          <div className="font-medium text-sm">{label}</div>
          {description && <div className="text-xs text-ide-muted mt-0.5">{description}</div>}
        </div>
      </div>
      <div className="flex items-center gap-2">
        <input
          type="text"
          disabled
          value={value || '--'}
          className="flex-1 bg-ide-panel border border-ide-border rounded-lg px-3 py-2 text-sm text-ide-muted truncate disabled:opacity-100 disabled:cursor-not-allowed"
        />
        <button
          type="button"
          onClick={onChangePath}
          disabled={disabled}
          className="shrink-0 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          {disabled ? t('settings.storageManagement.storagePath.changing') : t('settings.storageManagement.storagePath.label')}
        </button>
      </div>
      {error && <div className="mt-2 text-xs text-ide-error">{error}</div>}
    </div>
  );
}
