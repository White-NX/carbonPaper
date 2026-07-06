import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTauriEventListener } from './useTauriEventListener';

function normalizeDiscoveredPythonVersions(result) {
  const versions = Array.isArray(result) ? result : [result].filter(Boolean);

  return versions.map((pv, idx) => {
    const isPath = typeof pv === 'string' && (pv.includes('\\') || pv.includes('/') || String(pv).toLowerCase().endsWith('.exe'));
    if (isPath) {
      const path = pv;
      const filename = path.split(/[\\/]/).pop();
      return {
        id: `py${path.replace(/[^a-zA-Z0-9]/g, '') || idx}`,
        display: `${filename} - ${path}`,
        disabled: false,
        path,
      };
    }

    const verDigits = pv ? pv.replace(/\./g, '') : `${idx}`;
    const path = `C:\\Python${verDigits}\\python.exe`;
    return {
      id: `py${verDigits}`,
      display: `Python ${pv} - ${path}`,
      disabled: false,
      path,
    };
  });
}

export function useVenvInstallController({
  onRefreshPythonVersion,
  handleStartBackend,
  t,
}) {
  const [venvInstallStep, setVenvInstallStep] = useState(null);
  const [pythonPath, setPythonPath] = useState('');
  const [discoveredOptions, setDiscoveredOptions] = useState([]);
  const [selectedVersions, setSelectedVersions] = useState([]);
  const [versionErrorState, setVersionErrorState] = useState(null);
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState(null);
  const [installLogs, setInstallLogs] = useState([]);
  const [depsInstalling, setDepsInstalling] = useState(false);
  const [depsInstallLog, setDepsInstallLog] = useState([]);
  const [depsError, setDepsError] = useState(null);
  const [depsInstallSuccess, setDepsInstallSuccess] = useState(false);
  const [chosenPythonForInstall, setChosenPythonForInstall] = useState(null);

  const installLogRef = useRef(null);
  const depsLogRef = useRef(null);
  const installStartedRef = useRef(false);
  const pythonPathRef = useRef(pythonPath);
  const inputRef = useRef(null);
  const autoPostInstallRef = useRef(false);

  const appendInstallLog = (msg) => {
    setInstallLogs((prev) => [...prev, msg]);
  };

  const appendDepsLog = (msg) => {
    const ts = new Date().toLocaleTimeString();
    setDepsInstallLog((prev) => [...prev, `[${ts}] ${msg}`]);
  };

  useTauriEventListener('install-log', (event) => {
    const payload = event?.payload || {};
    const line = payload.line || JSON.stringify(payload);
    const source = payload.source || 'installer';
    if (source === 'pip' || source === 'aria2') {
      appendDepsLog(line);
    } else {
      appendInstallLog(line);
    }
  });

  useEffect(() => {
    if (venvInstallStep === 1) {
      let cancelled = false;
      setVersionErrorState(null);
      (async () => {
        try {
          setDiscoveredOptions([]);
          const result = await invoke('check_python_status');
          const opts = normalizeDiscoveredPythonVersions(result);
          if (!cancelled) setDiscoveredOptions(opts);
        } catch (error) {
          if (!cancelled) setVersionErrorState(error?.message || String(error));
        }
      })();
      return () => { cancelled = true; };
    }
  }, [venvInstallStep]);

  useEffect(() => {
    pythonPathRef.current = pythonPath;
  }, [pythonPath]);

  useEffect(() => {
    if (venvInstallStep === 2) {
      setDepsInstallLog([]);
      setDepsError(null);
      setDepsInstallSuccess(false);
      autoPostInstallRef.current = false;

      if (window.__cp_install_started) {
        appendDepsLog('安装已在进行中（忽略重复触发）');
        return;
      }
      window.__cp_install_started = true;
      installStartedRef.current = true;

      setDepsInstalling(true);

      const currentPythonPath = (chosenPythonForInstall !== null && chosenPythonForInstall !== undefined)
        ? chosenPythonForInstall
        : (inputRef.current?.value || pythonPathRef.current);
      const processedPythonPath = currentPythonPath ? currentPythonPath.replace(/\\/g, '\\\\') : null;

      appendDepsLog(t('mask.venv.step2.log_start'));
      appendDepsLog(t('mask.venv.step2.using_python', { path: processedPythonPath || t('mask.venv.step2.using_python_default') }));

      (async () => {
        try {
          const res = await invoke('install_python_venv', { python_path: processedPythonPath });
          appendDepsLog(res);
          const config = await invoke('get_advanced_config');
          const useOnnx = config?.use_onnx || false;

          appendDepsLog(t('mask.venv.step2.download_models'));
          const modelRes = await invoke('download_model');
          appendDepsLog(modelRes);

          appendDepsLog(t('mask.venv.step2.download_bge'));
          const bgeRes = useOnnx
            ? await invoke('download_model', {
              repo: 'Xenova/bge-small-zh-v1.5',
              subdir: 'bge-small-zh-v1.5',
              files: ['config.json', 'tokenizer.json', 'tokenizer_config.json', 'special_tokens_map.json', 'onnx/model_quantized.onnx'],
              modelRuntime: 'onnx',
            })
            : await invoke('download_model', {
              repo: 'BAAI/bge-small-zh-v1.5',
              subdir: 'bge-small-zh-v1.5',
              files: ['config.json', 'pytorch_model.bin', 'tokenizer.json', 'tokenizer_config.json', 'vocab.txt', 'special_tokens_map.json'],
            });
          appendDepsLog(bgeRes);

          appendDepsLog(t('mask.venv.step2.download_minilm'));
          const minilmRes = useOnnx
            ? await invoke('download_model', {
              repo: 'Xenova/paraphrase-multilingual-MiniLM-L12-v2',
              subdir: 'paraphrase-multilingual-MiniLM-L12-v2',
              files: ['config.json', 'tokenizer.json', 'tokenizer_config.json', 'special_tokens_map.json', 'onnx/model_quantized.onnx'],
              modelRuntime: 'onnx',
            })
            : await invoke('download_model', {
              repo: 'sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2',
              subdir: 'paraphrase-multilingual-MiniLM-L12-v2',
              files: ['config.json', 'pytorch_model.bin', 'tokenizer.json', 'tokenizer_config.json', 'special_tokens_map.json', 'sentencepiece.bpe.model'],
            });
          appendDepsLog(minilmRes);
          appendDepsLog(t('mask.venv.step2.deps_complete'));
          setDepsInstallSuccess(true);
        } catch (err) {
          appendDepsLog(t('mask.venv.step2.deps_failed', { error: err?.message || String(err) }));
          setDepsError(err?.message || String(err));
          setDepsInstallSuccess(false);
        } finally {
          setDepsInstalling(false);
          window.__cp_install_started = false;
          installStartedRef.current = false;
        }
      })();
    } else {
      installStartedRef.current = false;
      if (window.__cp_install_started) window.__cp_install_started = false;
    }
  }, [venvInstallStep, chosenPythonForInstall, t]);

  useEffect(() => {
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

  useEffect(() => {
    if (depsLogRef?.current) {
      depsLogRef.current.scrollTop = depsLogRef.current.scrollHeight;
    }
  }, [depsInstallLog]);

  useEffect(() => {
    if (installLogRef?.current) {
      installLogRef.current.scrollTop = installLogRef.current.scrollHeight;
    }
  }, [installLogs]);

  const toggleVersion = (id) => {
    const opt = discoveredOptions.find((o) => o.id === id);
    if (!opt || opt.disabled) return;
    if (selectedVersions.includes(id)) {
      setSelectedVersions(selectedVersions.filter((item) => item !== id));
    } else {
      setSelectedVersions([...selectedVersions, id]);
      setPythonPath(opt.path || '');
    }
  };

  const installPython = async () => {
    setInstallError(null);
    setInstalling(true);
    setInstallLogs([]);
    appendInstallLog(t('mask.venv.auto_install.log_start'));

    try {
      await invoke('request_install_python');
      appendInstallLog(t('mask.venv.auto_install.success'));
      appendInstallLog(t('mask.venv.auto_install.finished'));
      setVenvInstallStep(null);
      await invoke('close_process');
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
        filters: [{ name: 'Executable', extensions: ['exe'] }],
      });
      if (!selected) return;
      const chosen = Array.isArray(selected) ? selected[0] : selected;
      setPythonPath(chosen);
    } catch (err) {
      console.error('Failed to open file dialog', err);
    }
  };

  const beginDependencyInstall = () => {
    setChosenPythonForInstall(inputRef.current?.value || pythonPath);
    setVenvInstallStep(2);
  };

  const retryDependencyInstall = () => {
    setDepsError(null);
    setDepsInstallLog([]);
    setDepsInstallSuccess(false);
    window.__cp_install_started = false;
    installStartedRef.current = false;
    setVenvInstallStep(null);
    setTimeout(() => setVenvInstallStep(2), 0);
  };

  return {
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
  };
}
