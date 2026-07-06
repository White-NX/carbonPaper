import React from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Route, PackageOpen, Shield, RotateCcw, Download, X } from 'lucide-react';
import { useRequiredModelDownload } from '../hooks/useRequiredModelDownload';
import { useDepsSyncOverlay } from '../hooks/useDepsSyncOverlay';
import { useVenvInstallController } from '../hooks/useVenvInstallController';


export default function Mask({ backendStatus, pythonVersion, backendError, handleStartBackend, onRefreshPythonVersion, depsNeedUpdate, depsSyncing, onDepsSync, modelsNeedDownload, missingModels, onModelsDownloadComplete }) {
  const { t } = useTranslation();
  const {
    venvInstallStep,
    setVenvInstallStep,
    pythonPath,
    setPythonPath,
    discoveredOptions,
    selectedVersions,
    versionErrorState,
    installing,
    installError,
    installLogs,
    installLogRef,
    depsInstalling,
    depsInstallLog,
    depsError,
    depsLogRef,
    inputRef,
    toggleVersion,
    installPython,
    openFileDialog,
    beginDependencyInstall,
    retryDependencyInstall,
  } = useVenvInstallController({
    onRefreshPythonVersion,
    handleStartBackend,
    t,
  });

  // Debug helper: in dev mode show a dropdown to preview different masks
  const isDev = typeof import.meta !== 'undefined' && Boolean(import.meta.env && import.meta.env.DEV);
  const [debugMask, setDebugMask] = React.useState('none');
  const debugOverrides = React.useMemo(() => {
    switch (debugMask) {
      case 'backend-offline':
        return { backendStatus: 'offline', pythonVersion: '3.10.11', venvInstallStep: null };
      case 'backend-waiting':
        return { backendStatus: 'waiting', pythonVersion: '3.10.11', venvInstallStep: null };
      case 'venv-1':
        return { venvInstallStep: 1 };
      case 'venv-2':
        return { venvInstallStep: 2 };
      case 'venv-3':
        return { venvInstallStep: 3 };
      case 'no-python':
        return { venvInstallStep: null, pythonVersion: null };
      case 'online':
        return { backendStatus: 'online' };
      default:
        return {};
    }
  }, [debugMask]);

  const renderBackendStatus = debugOverrides.hasOwnProperty('backendStatus') ? debugOverrides.backendStatus : backendStatus;
  const renderPythonVersion = debugOverrides.hasOwnProperty('pythonVersion') ? debugOverrides.pythonVersion : pythonVersion;
  const renderVenvInstallStep = debugOverrides.hasOwnProperty('venvInstallStep') ? debugOverrides.venvInstallStep : venvInstallStep;
  const {
    modelDownloadLog,
    modelDownloadError,
    modelDownloading,
    modelDownloadLogRef,
    isClosedByUser,
    setIsClosedByUser,
    retryModelDownload,
  } = useRequiredModelDownload({
    modelsNeedDownload,
    missingModels,
    renderVenvInstallStep,
    depsNeedUpdate,
    onModelsDownloadComplete,
    t,
  });
  const {
    depsSyncLog,
    depsSyncError,
    depsSyncLogRef,
    retryDepsSync,
  } = useDepsSyncOverlay({
    depsNeedUpdate,
    pythonVersion,
    renderVenvInstallStep,
    depsSyncing,
    onDepsSync,
  });

  const renderDebugSelector = () => (
    <div className="absolute right-6 top-6 z-60 flex items-center gap-2">
      <label className="text-xs text-ide-muted">{t('mask.debug.label')}</label>
      <select
        value={debugMask}
        onChange={(e) => setDebugMask(e.target.value)}
        className="bg-ide-bg border border-ide-border rounded px-2 py-1 text-xs"
      >
        <option value="none">{t('mask.debug.none')}</option>
        <option value="online">{t('mask.debug.online')}</option>
        <option value="backend-offline">{t('mask.debug.offline')}</option>
        <option value="backend-waiting">{t('mask.debug.waiting')}</option>
        <option value="venv-1">{t('mask.debug.venv1')}</option>
        <option value="venv-2">{t('mask.debug.venv2')}</option>
        <option value="venv-3">{t('mask.debug.venv3')}</option>
        <option value="no-python">{t('mask.debug.nopython')}</option>
      </select>
    </div>
  );

  // ==================== Deps update overlay ====================
  if (depsNeedUpdate && renderPythonVersion && renderVenvInstallStep == null) {
    return (
      <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
        {isDev && renderDebugSelector()}
        <PackageOpen className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-semibold">{t('mask.deps_update.title')}</p>
        <p className="text-sm opacity-70 mt-1">{t('mask.deps_update.subtitle')}</p>

        <div className="mt-6 w-full max-w-2xl px-6">
          <textarea
            ref={depsSyncLogRef}
            readOnly
            value={depsSyncLog.join('\n')}
            rows={10}
            className={`w-full bg-ide-bg border ${depsSyncError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${depsSyncError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
          />
          {depsSyncError ? (
            <div className="mt-3 flex items-center gap-3">
              <span className="text-xs text-rose-400">{t('mask.deps_update.failed', { error: depsSyncError })}</span>
              <button
                onClick={retryDepsSync}
                className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
              >
                <RotateCcw className="w-3 h-3" />
                {t('mask.deps_update.retry')}
              </button>
            </div>
          ) : (
            <div className="mt-3 flex items-center gap-2 text-xs text-ide-muted">
              <Loader2 className="w-4 h-4 animate-spin" />
              {t('mask.deps_update.syncing')}
            </div>
          )}
        </div>
      </div>
    );
  }

  if (renderBackendStatus === 'online' && !modelsNeedDownload) return null;

  // ==================== Model download overlay ====================
  if (modelsNeedDownload && renderPythonVersion && renderVenvInstallStep == null && !depsNeedUpdate) {
    if (isClosedByUser) return null;
    return (
      <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
        {isDev && renderDebugSelector()}
        <Download className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-semibold">{t('mask.model_download.title')}</p>
        <p className="text-sm opacity-70 mt-1">{t('mask.model_download.subtitle')}</p>

        <div className="mt-6 w-full max-w-2xl px-6 relative">
          <button
            onClick={() => setIsClosedByUser(true)}
            className="absolute -top-10 right-6 text-ide-muted hover:text-ide-text transition-colors flex items-center gap-1 text-xs px-2.5 py-1 bg-ide-panel border border-ide-border rounded-md"
            title={t('mask.model_download.run_in_background', '后台运行')}
          >
            <X size={14} />
            {t('mask.model_download.run_in_background', '后台运行')}
          </button>
          <textarea
            ref={modelDownloadLogRef}
            readOnly
            value={modelDownloadLog.join('\n')}
            rows={10}
            className={`w-full bg-ide-bg border ${modelDownloadError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${modelDownloadError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
          />
          {modelDownloadError ? (
            <div className="mt-3 flex items-center gap-3">
              <span className="text-xs text-rose-400">{t('mask.model_download.failed', { error: modelDownloadError })}</span>
              <button
                onClick={retryModelDownload}
                className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
              >
                <RotateCcw className="w-3 h-3" />
                {t('mask.model_download.retry')}
              </button>
            </div>
          ) : (
            <div className="mt-3 flex items-center gap-2 text-xs text-ide-muted">
              <Loader2 className="w-4 h-4 animate-spin" />
              {t('mask.model_download.syncing')}
            </div>
          )}
        </div>
      </div>
    );
  }

  // Backend offline/waiting when pythonVersion exists — no longer show fullscreen mask.
  // The TopBar status badge now handles this state.
  if (renderBackendStatus !== 'online' && renderPythonVersion != null && renderVenvInstallStep == null) {
    return null;
  }

  // Install step 1: let user choose/provide python path and optionally auto-install
  if (renderVenvInstallStep === 1) {
    const canNext = (pythonPath && pythonPath.trim()) || selectedVersions.length > 0;

    return (
      <div className="absolute inset-0 z-50 flex flex-col items-start justify-start bg-ide-bg/80 backdrop-blur-sm text-ide-muted p-6">
        {isDev && renderDebugSelector()}
        <div className="w-full flex items-start justify-between">
          <div>
            <h2 className="text-2xl font-bold text-ide-text">{t('mask.venv.step1.title', { version: '3.10.11' })}</h2>
            <p className="text-sm text-ide-muted mt-1">{t('mask.venv.step1.subtitle')}</p>
          </div>
        </div>

        <div className="mt-6 w-full max-w-xl">
          <label className="text-xs text-ide-muted">{t('mask.venv.pythonPath.label')}</label>
          <div className="flex items-center bg-ide-bg border border-ide-border rounded-md overflow-hidden mt-1">
            <input
              ref={inputRef}
              type="text"
              className="bg-transparent text-sm px-3 py-2 focus:outline-none w-full"
              placeholder={t('mask.venv.pythonPath.placeholder')}
              value={pythonPath}
              onChange={(e) => setPythonPath(e.target.value)}
            />
            <button
              type="button"
              className="px-3 text-xs text-blue-300 hover:text-blue-200" onClick={openFileDialog}
            >{t('mask.venv.choose')}</button>
          </div>

          <div className="mt-4">
            <label className="text-xs text-ide-muted">{t('mask.venv.discovered_label')}</label>
            <div className="mt-2 flex flex-col gap-1">
              {discoveredOptions.map((opt) => (
                <label key={opt.id} className={`flex items-center justify-between gap-2 text-xs px-2 py-2 rounded hover:bg-ide-hover/30 ${opt.disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
                  <span className="flex items-center gap-2">
                    <input
                      type="radio"
                      className="accent-blue-400"
                      checked={selectedVersions.includes(opt.id)}
                      disabled={opt.disabled}
                      onChange={() => toggleVersion(opt.id)}
                    />
                    <span className="truncate">{opt.display}</span>
                  </span>
                  {opt.disabled && <span className="text-rose-400 text-[11px]">{t('mask.venv.unsupported')}</span>}
                </label>
              ))}
              {discoveredOptions.length === 0 && (<div className="text-xs text-ide-muted px-2 py-3">{t('mask.venv.not_found')}</div>)}
            </div>
          </div>
          <label className="text-xs text-ide-muted">{t('mask.venv.note')}</label>
          <div className="mt-4 flex items-center gap-3">
            <button
              onClick={installPython}
              disabled={installing || discoveredOptions.length !== 0}
              className="flex items-center gap-2 px-3 py-1.5 bg-green-600 hover:bg-green-700 text-white rounded text-xs transition-colors disabled:opacity-50"
            >
              {installing ? <Loader2 className="w-4 h-4 animate-spin" /> : <Shield className="w-3 h-3" />}
              {installing ? t('mask.venv.auto_install.installing') : t('mask.venv.auto_install.button')}
            </button>
            {installError && <span className="text-xs text-red-400">{installError}</span>}
          </div>

          {installing && (
            <div className="mt-4 w-full max-w-xl">
              <label className="text-xs text-ide-muted">{t('mask.venv.install_logs')}</label>
              <div className="mt-2">
                <textarea
                  ref={installLogRef}
                  readOnly
                  value={installLogs.join('\n')}
                  rows={6}
                  className={`w-full bg-ide-bg border ${installError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${installError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
                />
              </div>
              {installError && <div className="mt-2 text-xs text-rose-400">{t('mask.venv.install_failed', { error: installError })}</div>}
            </div>
          )}
        </div>

        <div className="absolute right-6 bottom-6">
            <button
            onClick={beginDependencyInstall}
            disabled={!canNext || installing}
            className="flex items-center gap-2 px-3 py-1.5 bg-blue-600 hover:bg-blue-700 text-white rounded text-sm transition-colors disabled:opacity-50"
          >
            {t('mask.venv.next')}
          </button>
        </div>
      </div>
    );
  }

  // Install step 2: create venv and install requirements
  if (renderVenvInstallStep === 2) {

    return (
      <div className="absolute inset-0 z-50 flex flex-col items-start justify-start bg-ide-bg/80 backdrop-blur-sm text-ide-muted p-6">
        {isDev && renderDebugSelector()}
        <div className="w-full flex items-start justify-between">
          <div>
            <h2 className="text-2xl font-bold text-ide-text">{t('mask.venv.step2.title')}</h2>
            <p className="text-sm text-ide-muted mt-1">{t('mask.venv.step2.subtitle')}</p>
            <p className="text-xs text-ide-muted mt-2">{t('mask.venv.step2.disk_note')}</p>
          </div>
        </div>

        <div className="mt-6 w-full max-w-3xl">
          <label className="text-xs text-ide-muted">{t('mask.venv.step2.install_logs')}</label>
          <div className="mt-2">
            <textarea
              ref={depsLogRef}
              readOnly
              value={depsInstallLog.join('\n')}
              rows={12}
              className={`w-full bg-ide-bg border ${depsError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${depsError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
            />
          </div>
          {depsError ? (
              <div className="mt-3 flex items-center gap-3">
              <span className="text-xs text-rose-400">{t('mask.venv.step2.install_failed')}</span>
              <button
                onClick={retryDependencyInstall}
                className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
              >
                <RotateCcw className="w-3 h-3" />
                {t('mask.venv.step2.retry')}
              </button>
            </div>
          ) : (
            <div className="mt-3 text-xs text-ide-muted">{t('mask.venv.step2.running')}</div>
          )}
        </div>

        <div className="absolute right-6 bottom-6 flex items-center gap-2">
          <button
            onClick={() => setVenvInstallStep(3)}
            disabled={depsInstalling || depsError !== null}
            className="flex items-center gap-2 px-3 py-1.5 bg-blue-600 hover:bg-blue-700 text-white rounded text-sm transition-colors disabled:opacity-50"
          >
            {t('mask.venv.step2.complete')}
          </button>
        </div>
      </div>
    );
  }

  if (!renderVenvInstallStep && !renderPythonVersion) {
    return (
      <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
        {isDev && renderDebugSelector()}
        <Route className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-semibold">{t('mask.first_run.title')}</p>
        <p className="text-sm opacity-70">{t('mask.first_run.description')}</p>
        <div className="flex items-center gap-3 mt-4">
          <button
            onClick={() => { setVenvInstallStep(1); }}
            disabled={renderBackendStatus === 'waiting' && Boolean(renderPythonVersion)}
            className="flex items-center gap-2 px-3 py-1.5 bg-green-600 hover:bg-green-700 text-white rounded text-xs transition-colors disabled:opacity-50"
          >
            <PackageOpen className="w-3 h-3" />{t('mask.first_run.install_button')}
          </button>
          <span className="text-xs text-ide-muted">{pythonVersion}</span>
        </div>
      </div>
    );
  }
}
