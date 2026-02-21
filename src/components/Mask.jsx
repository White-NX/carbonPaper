import React from 'react';
import { useTranslation } from 'react-i18next';
import { WifiOff, Loader2, Play, Route, PackageOpen, Shield, ShieldEllipsis, RotateCcw } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';


export default function Mask({ backendStatus, pythonVersion, backendError, handleStartBackend, onRefreshPythonVersion, depsNeedUpdate, depsSyncing, onDepsSync }) {

  const [venvInstallStep, setVenvInstallStep] = React.useState(null);
  const [pythonPath, setPythonPath] = React.useState('');
  const [discoveredOptions, setDiscoveredOptions] = React.useState([]);
  const [selectedVersions, setSelectedVersions] = React.useState([]);
  const [versionErrorState, setVersionErrorState] = React.useState(null);
  const [installing, setInstalling] = React.useState(false);
  const [installError, setInstallError] = React.useState(null);

  // Step 1: install button logs & helper (for auto-installing system Python)
  const [installLogs, setInstallLogs] = React.useState([]);
  const installLogRef = React.useRef(null);
  const appendInstallLog = (msg) => {
    setInstallLogs((prev) => [...prev, msg]);
  };

  // Global listener ref for backend install logs
  const installListenerRef = React.useRef(null);

  // Listen for 'install-log' events emitted from the Rust backend and route them
  // to the appropriate log view (installer vs pip). This is registered once on
  // mount and removed on unmount.
  React.useEffect(() => {
    let mounted = true;
    (async () => {
      try {
        const unlisten = await listen('install-log', (event) => {
          const payload = event?.payload || {};
          const line = payload.line || JSON.stringify(payload);
          const source = payload.source || 'installer';
          // Route pip source to dependency logs, everything else goes to install logs
          if (source === 'pip' || source === 'aria2') { 
            appendDepsLog(line);
          } else {
            appendInstallLog(line);
          }
        });
        installListenerRef.current = unlisten;
      } catch (e) {
        console.warn('Failed to register install-log listener', e);
      }
    })();

    return () => {
      if (installListenerRef.current) {
        try {
          installListenerRef.current();
        } catch (e) {
          console.warn('Failed to remove install-log listener', e);
        }
      }
    };
  }, []);

  // Step 2: dependency installation (venv + pip install)
  const [depsInstalling, setDepsInstalling] = React.useState(false);
  const [depsInstallLog, setDepsInstallLog] = React.useState([]);
  const [depsError, setDepsError] = React.useState(null);
  const [depsInstallSuccess, setDepsInstallSuccess] = React.useState(false);
  const depsLogRef = React.useRef(null);

  // fetch discovered python installations asynchronously
  const getDiscoveredPythonVersions = async () => {
    const result = await invoke('check_python_status');
    console.log(result);

    // normalize result to an array of version strings
    const versions = Array.isArray(result) ? result : [result].filter(Boolean);

    return versions.map((pv, idx) => {
      // if backend returned a full path (e.g. C:\\Program Files\\Python310\\python.exe), use it directly
      const isPath = typeof pv === 'string' && (pv.includes('\\') || pv.includes('/') || String(pv).toLowerCase().endsWith('.exe'));
      if (isPath) {
        const path = pv;
        const filename = path.split(/[\\\/]/).pop();
        return {
          id: `py${path.replace(/[^a-zA-Z0-9]/g, '') || idx}`,
          display: `${filename} - ${path}`,
          disabled: false,
          path,
        };
      }

      // otherwise assume pv is a version string like "3.10.11"
      const verDigits = pv ? pv.replace(/\./g, '') : `${idx}`;
      const path = `C:\\Python${verDigits}\\python.exe`;
      return {
        id: `py${verDigits}`,
        display: `Python ${pv} - ${path}`,
        disabled: false,
        path,
      };
    });
  };

  React.useEffect(() => {
    if (venvInstallStep === 1) {
      let cancelled = false;
      setVersionErrorState(null);
      (async () => {
        try {
          setDiscoveredOptions([]); // clear while loading
          const opts = await getDiscoveredPythonVersions();
          if (!cancelled) setDiscoveredOptions(opts);
        } catch (error) {
          if (!cancelled) setVersionErrorState(error?.message || String(error));
        }
      })();
      return () => { cancelled = true; };
    }
  }, [venvInstallStep]);

  // Automatically run dependency installation when entering step 2
  const appendDepsLog = (msg) => {
    const ts = new Date().toLocaleTimeString();
    setDepsInstallLog((prev) => [...prev, `[${ts}] ${msg}`]);
  };

  // Use a ref to track if installation has already started to prevent double execution
  const installStartedRef = React.useRef(false);

  // Store pythonPath in a ref so the effect always has access to the latest value
  const pythonPathRef = React.useRef(pythonPath);
  React.useEffect(() => {
    pythonPathRef.current = pythonPath;
  }, [pythonPath]);

  // Reference to the actual input element so we can synchronously read the user's typed/selected path
  const inputRef = React.useRef(null);
  // Capture the python path that the user intended to use when they click "Next"
  const [chosenPythonForInstall, setChosenPythonForInstall] = React.useState(null);
  
  const { t } = useTranslation();

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

  const autoPostInstallRef = React.useRef(false);

  // ==================== Deps update overlay state ====================
  const [depsSyncLog, setDepsSyncLog] = React.useState([]);
  const [depsSyncError, setDepsSyncError] = React.useState(null);
  const [depsSyncStarted, setDepsSyncStarted] = React.useState(false);
  const depsSyncLogRef = React.useRef(null);

  // Auto-scroll deps sync log
  React.useEffect(() => {
    if (depsSyncLogRef?.current) {
      depsSyncLogRef.current.scrollTop = depsSyncLogRef.current.scrollHeight;
    }
  }, [depsSyncLog]);

  // Auto-start deps sync when overlay is shown
  React.useEffect(() => {
    if (!depsNeedUpdate || !pythonVersion || renderVenvInstallStep != null) return;
    if (depsSyncStarted || depsSyncing) return;

    setDepsSyncStarted(true);
    setDepsSyncLog([]);
    setDepsSyncError(null);

    (async () => {
      try {
        await onDepsSync();
        setDepsSyncStarted(false);
      } catch (err) {
        setDepsSyncError(err?.message || String(err));
        setDepsSyncStarted(false);
      }
    })();
  }, [depsNeedUpdate, pythonVersion, renderVenvInstallStep, depsSyncStarted, depsSyncing, onDepsSync]);

  // Listen for install-log events to populate deps sync log
  // (We reuse the existing listener — route 'pip' and 'installer' source lines to depsSyncLog when syncing)
  const prevDepsNeedUpdate = React.useRef(false);
  React.useEffect(() => {
    // When depsNeedUpdate transitions from false to true, start capturing logs
    if (depsNeedUpdate && !prevDepsNeedUpdate.current) {
      setDepsSyncLog([]);
      setDepsSyncError(null);
    }
    prevDepsNeedUpdate.current = depsNeedUpdate;
  }, [depsNeedUpdate]);

  // Capture install-log events into depsSyncLog when deps are syncing
  React.useEffect(() => {
    if (!depsNeedUpdate && !depsSyncing) return;
    let mounted = true;
    let unlisten;
    (async () => {
      try {
        unlisten = await listen('install-log', (event) => {
          if (!mounted) return;
          const payload = event?.payload || {};
          const line = payload.line || JSON.stringify(payload);
          const ts = new Date().toLocaleTimeString();
          setDepsSyncLog((prev) => [...prev, `[${ts}] ${line}`]);
        });
      } catch (e) {
        console.warn('Failed to register deps sync log listener', e);
      }
    })();
    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [depsNeedUpdate, depsSyncing]);

  React.useEffect(() => {
    if (venvInstallStep === 2) {
      // Clear previous logs/errors so each run is fresh
      setDepsInstallLog([]);
      setDepsError(null);
      setDepsInstallSuccess(false);
      autoPostInstallRef.current = false;

      // Prevent double execution (handles StrictMode/HMR by using a global flag)
      if (window.__cp_install_started) {
        appendDepsLog('安装已在进行中（忽略重复触发）');
        return;
      }
      window.__cp_install_started = true;
      installStartedRef.current = true;

      setDepsInstalling(true);

      // Prefer the value captured when the user clicked 'Next' (synchronously read from input),
      // otherwise fall back to the input DOM value, then the ref value.
      const currentPythonPath = (chosenPythonForInstall !== null && chosenPythonForInstall !== undefined)
        ? chosenPythonForInstall
        : (inputRef.current?.value || pythonPathRef.current);

      // Escape backslashes by doubling them, since backend expects escaped backslashes
      const processedPythonPath = currentPythonPath ? currentPythonPath.replace(/\\/g, '\\\\') : null;

      // append initial log and perform an invoke to the backend
      appendDepsLog(t('mask.venv.step2.log_start'));
      appendDepsLog(t('mask.venv.step2.using_python', { path: processedPythonPath || t('mask.venv.step2.using_python_default') }));

      (async () => {
        try {
          console.log('Calling install_python_venv invoke...', { python_path: processedPythonPath });
          // try to call monitor IPC to ask it to install requirements
          // note: the monitor may not be running yet; errors are caught and logged
          // pass the python executable path chosen in the previous step (escaped)
          const res = await invoke('install_python_venv', { python_path: processedPythonPath });
          appendDepsLog(res);
          // download model files
          appendDepsLog(t('mask.venv.step2.download_models'));
          const modelRes = await invoke('download_model');
          appendDepsLog(modelRes);
          appendDepsLog(t('mask.venv.step2.deps_complete'));
          setDepsInstallSuccess(true);
        } catch (err) {
          appendDepsLog(t('mask.venv.step2.deps_failed', { error: err?.message || String(err) }));
          setDepsError(err?.message || String(err));
          setDepsInstallSuccess(false);
        } finally {
          setDepsInstalling(false);
          window.__cp_install_started = false; // allow retries
          installStartedRef.current = false;
        }
      })();
    } else {
      // Reset the ref and global flag when leaving step 2
      installStartedRef.current = false;
      if (window.__cp_install_started) window.__cp_install_started = false;
    }
    // no cleanup required, we do not use intervals here
  }, [venvInstallStep]);

  React.useEffect(() => {
    if (depsInstalling || depsError || !depsInstallSuccess) return;
    if (autoPostInstallRef.current) return;
    autoPostInstallRef.current = true;

    (async () => {
      try {
        if (typeof onRefreshPythonVersion === 'function') {
          await onRefreshPythonVersion();
        }
      } catch (e) {
        console.warn('Failed to refresh Python version after install', e);
      } finally {
        setVenvInstallStep(null);
        if (typeof handleStartBackend === 'function') {
          handleStartBackend();
        }
      }
    })();
  }, [depsInstalling, depsError, depsInstallSuccess, handleStartBackend, onRefreshPythonVersion]);

  React.useEffect(() => {
    if (depsLogRef?.current) {
      depsLogRef.current.scrollTop = depsLogRef.current.scrollHeight;
    }
  }, [depsInstallLog]);

  // scroll install button log
  React.useEffect(() => {
    if (installLogRef?.current) {
      installLogRef.current.scrollTop = installLogRef.current.scrollHeight;
    }
  }, [installLogs]);

  const toggleVersion = (id) => {
    const opt = discoveredOptions.find((o) => o.id === id);
    if (!opt || opt.disabled) return;
    if (selectedVersions.includes(id)) {
      setSelectedVersions(selectedVersions.filter((s) => s !== id));
    } else {
      setSelectedVersions([...selectedVersions, id]);
      // also set path to selected candidate for convenience
      setPythonPath(opt.path || '');
    }
  };

  const installPython = async () => {
    setInstallError(null);
    setInstalling(true);
    setInstallLogs([]);
    appendInstallLog(t('mask.venv.auto_install.log_start'));

    try {

      let result = await invoke('request_install_python');
      appendInstallLog(t('mask.venv.auto_install.success'));

      appendInstallLog(t('mask.venv.auto_install.finished'));
      setVenvInstallStep(null);
      let _ = await invoke('close_process');
    } catch (err) {
      setInstallError(t('mask.venv.install_failed', { error: err?.message || String(err) }));
      appendInstallLog(t('mask.venv.install_failed', { error: err?.message || String(err) }));
    } finally {
      setInstalling(false);
    }
  };

  const openFileDialog = async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: 'Executable', extensions: ['exe'] }]
      });
      if (!selected) return;
      const chosen = Array.isArray(selected) ? selected[0] : selected;
      setPythonPath(chosen);
    } catch (err) {
      console.error('Failed to open file dialog', err);
    }
  };

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
                onClick={() => {
                  setDepsSyncError(null);
                  setDepsSyncStarted(false);
                }}
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

  if (renderBackendStatus === 'online') return null;

  // Two states from original App.jsx
  if (renderBackendStatus !== 'online' && renderPythonVersion != null && renderVenvInstallStep == null) {
    return (
      <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
        {isDev && renderDebugSelector()}
        <WifiOff className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-semibold">{t('mask.backend_shutdown.title')}</p>
        <p className="text-sm opacity-70">{t('mask.backend_shutdown.description')}</p>
        <div className="flex items-center gap-3 mt-4">
          <button
            onClick={handleStartBackend}
            disabled={renderBackendStatus === 'waiting'}
            className="flex items-center gap-2 px-3 py-1.5 bg-green-600 hover:bg-green-700 text-white rounded text-xs transition-colors disabled:opacity-50"
          >
            {renderBackendStatus === 'waiting' ? <Loader2 className="w-4 h-4 animate-spin" /> : <Play className="w-3 h-3 fill-current" />}
            {renderBackendStatus === 'waiting' ? t('mask.start.starting') : t('mask.start.start_service')}
          </button>
          {renderBackendStatus === 'waiting' && <span className="text-xs text-ide-muted">{t('mask.start.waiting')}</span>}
        </div>
        {backendError && <p className="text-xs text-red-400 mt-2">{backendError}</p>}
      </div>
    );
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
            onClick={() => { setChosenPythonForInstall(inputRef.current?.value || pythonPath); setVenvInstallStep(2); }}
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
                onClick={() => {
                  setDepsError(null);
                  setDepsInstallLog([]);
                  setDepsInstallSuccess(false);
                  window.__cp_install_started = false;
                  installStartedRef.current = false;
                  // Re-trigger the installation effect by toggling step away and back
                  setVenvInstallStep(null);
                  setTimeout(() => setVenvInstallStep(2), 0);
                }}
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
