import React from 'react';
import { useTranslation } from 'react-i18next';
import { Clock, Database, FolderOpen } from 'lucide-react';
import StorageOptionSelect from './StorageOptionSelect';
import StoragePathOption from './StoragePathOption';

export default function StoragePolicyGrid({
  currentStoragePath,
  migrationError,
  isUpdatingStoragePath,
  isMigrating,
  storageLimit,
  setStorageLimit,
  storageLimitOptions,
  retentionPeriod,
  setRetentionPeriod,
  retentionOptions,
  onChangeStoragePath,
}) {
  const { t } = useTranslation();

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
      <StoragePathOption
        className="md:col-span-2"
        label={t('settings.storageManagement.storagePath.label')}
        description={t('settings.storageManagement.storagePath.description')}
        value={currentStoragePath}
        onChangePath={onChangeStoragePath}
        icon={FolderOpen}
        error={migrationError}
        disabled={isUpdatingStoragePath || isMigrating}
      />

      <StorageOptionSelect
        label={t('settings.storageManagement.storageLimit.label')}
        description={t('settings.storageManagement.storageLimit.description')}
        value={storageLimit}
        onChange={setStorageLimit}
        options={storageLimitOptions}
        icon={Database}
      />

      <StorageOptionSelect
        label={t('settings.storageManagement.retention.label')}
        description={t('settings.storageManagement.retention.description')}
        value={retentionPeriod}
        onChange={setRetentionPeriod}
        options={retentionOptions}
        icon={Clock}
      />
    </div>
  );
}
