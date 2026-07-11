import React from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw } from 'lucide-react';
import { SettingsErrorBanner, SettingsWarningBanner } from './SettingsPrimitives';
import IndexHealthCard from './storage/IndexHealthCard';
import ProcessDetailView from './storage/ProcessDetailView';
import ProcessStorageCard from './storage/ProcessStorageCard';
import StorageDialogs from './storage/StorageDialogs';
import StorageOverviewCard from './storage/StorageOverviewCard';
import StoragePolicyGrid from './storage/StoragePolicyGrid';
import { useStorageManagementController } from './useStorageManagementController';

export default function StorageManagementSection({
  storageSegments,
  totalStorage,
  storage,
  loading,
  refreshing,
  error,
  onRefresh,
  monitorStatus,
}) {
  const { t } = useTranslation();
  const {
    storageLimit,
    setStorageLimit,
    retentionPeriod,
    setRetentionPeriod,
    isMigrating,
    migrationProgress,
    migrationError,
    isUpdatingStoragePath,
    isMigrationChoiceDialogOpen,
    pendingTargetPath,
    panelView,
    setPanelView,
    processStats,
    processStatsLoading,
    processStatsError,
    selectedProcess,
    processPage,
    processMonthData,
    processMonthLoading,
    processMonthError,
    processThumbMap,
    selectedScreenshotIds,
    deletingTarget,
    pendingDeleteIntent,
    isBackupDialogOpen,
    setIsBackupDialogOpen,
    backupMode,
    setBackupMode,
    deleteQueueStatus,
    indexHealth,
    indexHealthLoading,
    indexHealthError,
    vectorRetrying,
    groupedMonthItems,
    selectedCountByMonth,
    storageLimitOptions,
    retentionOptions,
    diskInfo,
    currentStoragePath,
    vectorRetryBacklog,
    indexHealthDeleteQueuePending,
    lastIndexingError,
    lastIndexingErrorAt,
    storageIpcLabel,
    storageIpcRetryAfter,
    handleRefresh,
    loadIndexHealth,
    handleRetryVectorIndexing,
    formatIndexCount,
    openProcessDetail,
    toggleScreenshotSelection,
    requestSoftDelete,
    handleConfirmSoftDelete,
    handleCancelSoftDelete,
    loadProcessMonthPage,
    handleChangeStoragePath,
    handleCancelMigrationChoice,
    handleApplyStoragePath,
  } = useStorageManagementController({ storage, onRefresh, t, monitorStatus });

  const openBackupDialog = (mode) => {
    setBackupMode(mode);
    setIsBackupDialogOpen(true);
  };

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between shrink-0">
        <div className="space-y-1">
          <h2 className="text-xl font-semibold">{t('settings.storageManagement.title')}</h2>
          <p className="text-xs text-ide-muted">{t('settings.storageManagement.description')}</p>
        </div>
        <button
          type="button"
          onClick={handleRefresh}
          disabled={refreshing}
          className="flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          {t('settings.storageManagement.refresh')}
        </button>
      </div>

      {error && <SettingsErrorBanner>{error}</SettingsErrorBanner>}

      {panelView === 'overview' && (
        <>
          <StorageOverviewCard
            storageSegments={storageSegments}
            totalStorage={totalStorage}
            storage={storage}
            loading={loading}
            diskInfo={diskInfo}
            onExportBackup={() => openBackupDialog('export')}
            onImportBackup={() => openBackupDialog('import')}
          />

          <IndexHealthCard
            indexHealth={indexHealth}
            indexHealthLoading={indexHealthLoading}
            indexHealthError={indexHealthError}
            vectorRetrying={vectorRetrying}
            vectorRetryBacklog={vectorRetryBacklog}
            deleteQueuePending={indexHealthDeleteQueuePending}
            lastIndexingError={lastIndexingError}
            lastIndexingErrorAt={lastIndexingErrorAt}
            storageIpcLabel={storageIpcLabel}
            storageIpcRetryAfter={storageIpcRetryAfter}
            monitorStatus={monitorStatus}
            onRefresh={loadIndexHealth}
            onRetryVectorIndexing={handleRetryVectorIndexing}
            formatIndexCount={formatIndexCount}
          />

          <StoragePolicyGrid
            currentStoragePath={currentStoragePath}
            migrationError={migrationError}
            isUpdatingStoragePath={isUpdatingStoragePath}
            isMigrating={isMigrating}
            storageLimit={storageLimit}
            setStorageLimit={setStorageLimit}
            storageLimitOptions={storageLimitOptions}
            retentionPeriod={retentionPeriod}
            setRetentionPeriod={setRetentionPeriod}
            retentionOptions={retentionOptions}
            onChangeStoragePath={handleChangeStoragePath}
          />

          <ProcessStorageCard
            deleteQueueStatus={deleteQueueStatus}
            processStats={processStats}
            processStatsLoading={processStatsLoading}
            processStatsError={processStatsError}
            onOpenProcessDetail={openProcessDetail}
          />

          {storageLimit === 'unlimited' && retentionPeriod === 'permanent' && (
            <SettingsWarningBanner title={t('settings.storageManagement.warning.title')}>
              <p>{t('settings.storageManagement.warning.message')}</p>
            </SettingsWarningBanner>
          )}
        </>
      )}

      {panelView === 'process-detail' && (
        <ProcessDetailView
          selectedProcess={selectedProcess}
          processPage={processPage}
          processMonthData={processMonthData}
          processMonthLoading={processMonthLoading}
          processMonthError={processMonthError}
          processThumbMap={processThumbMap}
          selectedScreenshotIds={selectedScreenshotIds}
          deletingTarget={deletingTarget}
          groupedMonthItems={groupedMonthItems}
          selectedCountByMonth={selectedCountByMonth}
          onBack={() => setPanelView('overview')}
          onRequestSoftDelete={requestSoftDelete}
          onToggleScreenshotSelection={toggleScreenshotSelection}
          onLoadProcessMonthPage={loadProcessMonthPage}
        />
      )}

      <StorageDialogs
        pendingDeleteIntent={pendingDeleteIntent}
        deletingTarget={deletingTarget}
        onCancelSoftDelete={handleCancelSoftDelete}
        onConfirmSoftDelete={handleConfirmSoftDelete}
        isMigrating={isMigrating}
        migrationProgress={migrationProgress}
        migrationError={migrationError}
        isMigrationChoiceDialogOpen={isMigrationChoiceDialogOpen}
        pendingTargetPath={pendingTargetPath}
        onCancelMigrationChoice={handleCancelMigrationChoice}
        onApplyStoragePath={handleApplyStoragePath}
        isBackupDialogOpen={isBackupDialogOpen}
        onCloseBackupDialog={() => setIsBackupDialogOpen(false)}
        backupMode={backupMode}
      />
    </div>
  );
}
