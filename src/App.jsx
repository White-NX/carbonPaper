import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import {
  Moon, Sun, Settings, Bell, Terminal, Layout, Minus, Square, X, Copy, Loader2, Monitor, Clock,
  WifiOff, Play, Search as SearchIcon, Info as InfoIcon, Route, PackageOpen
} from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import Timeline from './components/Timeline';
import { InspectorImage } from './components/InspectorImage';
import { SearchBox } from './components/SearchBox';
import { AdvancedSearch } from './components/AdvancedSearch';
import SettingsDialog from './components/settings/SettingsDialog';
import Mask from './components/Mask';
import AuthMask from './components/AuthMask';
import SecurityAlertMask from './components/SecurityAlertMask';

import ExtensionSetupWizard from './components/ExtensionSetupWizard';
import ClusteringSetupWizard from './components/ClusteringSetupWizard';
import SmartClusterSetupWizard from './components/SmartClusterSetupWizard';
import ActivityBar from './components/ActivityBar';
import MainArea from './components/MainArea';
import TopBar from './components/TopBar';
import { NotificationToast, NotificationPanel } from './components/Notifications';
import ErrorWindow from './components/ErrorWindow';
import HmacMigrationDialog from './components/HmacMigrationDialog';
import StartupVacuumDialog from './components/StartupVacuumDialog';
import { getScreenshotDetails, fetchImage, deleteScreenshot, deleteRecordsByTimeRange } from './lib/monitor_api';
import { checkForUpdate, downloadAndInstallUpdate } from './lib/update_api';
import { UpdateModal } from './components/UpdateModal';
import { useDelayedClusteringSetupRunner } from './hooks/useDelayedClusteringSetupRunner';

function App() {
  // Disable context menu for Tauri production feel
  useEffect(() => {
    const handleContextMenu = (e) => {
      // Allow context menu only on input fields if needed, or disable globally:
      if (['INPUT', 'TEXTAREA'].includes(e.target.tagName)) return;
      e.preventDefault();
      return false;
    };
    document.addEventListener('contextmenu', handleContextMenu);
    return () => {
      document.removeEventListener('contextmenu', handleContextMenu);
    };
  }, []);

  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return localStorage.getItem('theme') === 'dark' ||
        (!localStorage.getItem('theme') && window.matchMedia('(prefers-color-scheme: dark)').matches);
    }
    return true; // Default to dark for IDE theme
  });

  const [showSettings, setShowSettings] = useState(false);
  const [autoStartMonitor, setAutoStartMonitor] = useState(() => {
    if (typeof window === 'undefined') return true;
    const saved = localStorage.getItem('autoStartMonitor');
    return saved === null ? true : saved === 'true';
  });
  const [autoStartSuppressed, setAutoStartSuppressed] = useState(false);

  // Power Saving Mode State (managed by Rust backend)
  const [powerSavingMode, setPowerSavingMode] = useState(() => {
    if (typeof window === 'undefined') return true;
    const saved = localStorage.getItem('powerSavingMode');
    return saved === null ? true : saved === 'true';
  });
  const [powerSavingSuppressed, setPowerSavingSuppressed] = useState(false);
  const [windowFocused, setWindowFocused] = useState(true);

  useEffect(() => {
    localStorage.setItem('powerSavingMode', powerSavingMode ? 'true' : 'false');
  }, [powerSavingMode]);

  // Listen for power-saving-changed events from Rust backend
  useEffect(() => {
    let unlisten;
    const setup = async () => {
      unlisten = await listen('power-saving-changed', (event) => {
        const payload = event.payload || {};
        // active = true means power saving is active (AC disconnected)
        setPowerSavingSuppressed(payload.active === true);
      });

      // Fetch initial status from Rust
      try {
        const status = await invoke('get_power_saving_status');
        setPowerSavingMode(status.enabled !== false);
        setPowerSavingSuppressed(status.active === true);
      } catch (err) {
        console.warn('Failed to get initial power saving status:', err);
      }
    };
    setup();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Window focus tracking for power saving SQL pause
  useEffect(() => {
    const appWindow = getCurrentWindow();
    const unlistenFocus = appWindow.listen('tauri://focus', () => setWindowFocused(true));
    const unlistenBlur = appWindow.listen('tauri://blur', () => setWindowFocused(false));
    return () => {
      unlistenFocus.then(f => f());
      unlistenBlur.then(f => f());
    };
  }, []);

  // Windows Hello Authentication State
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const [authError, setAuthError] = useState(null);

  const [showExtensionSetup, setShowExtensionSetup] = useState(false);
  const [showClusteringSetup, setShowClusteringSetup] = useState(false);
  const [showSmartClusterSetup, setShowSmartClusterSetup] = useState(false);
  const [sessionTimeout, setSessionTimeout] = useState(() => {
    const saved = localStorage.getItem('sessionTimeout');
    return saved ? parseInt(saved, 10) : 900; // 默认 15 分钟
  });

  // Selected Timeline Event State
  const [selectedEvent, setSelectedEvent] = useState(null);
  const [selectedDetails, setSelectedDetails] = useState(null);
  const [selectedImageSrc, setSelectedImageSrc] = useState(null);
  const [isLoadingDetails, setIsLoadingDetails] = useState(false);
  const [lastError, setLastError] = useState(null);
  const [highlightedEventId, setHighlightedEventId] = useState(null);
  const [backendStatus, setBackendStatus] = useState('unknown'); // 'online' | 'offline' | 'waiting'
  const [monitorPaused, setMonitorPaused] = useState(false);
  const [backendError, setBackendError] = useState('');
  const backendStatusRef = useRef('unknown');
  const backendStartAtRef = useRef(null);
  const lastBackendErrorRef = useRef('');
  const [activeTab, setActiveTab] = useState('preview'); // 'preview' | 'advanced-search' | 'tasks'
  const [sidebarExpanded, setSidebarExpanded] = useState(false);
  const [searchMode, setSearchMode] = useState('ocr');
  const [advancedSearchParams, setAdvancedSearchParams] = useState({ query: '', mode: 'ocr', refreshKey: Date.now() });
  const [timelineRefreshKey, setTimelineRefreshKey] = useState(0);

  // debug
  const [pythonVersion, setPythonVersion] = useState(null);

  // Dependency sync state
  const [depsNeedUpdate, setDepsNeedUpdate] = useState(false);
  const [depsSyncing, setDepsSyncing] = useState(false);
  const [depsCheckDone, setDepsCheckDone] = useState(false);

  // Model file check state
  const [modelsNeedDownload, setModelsNeedDownload] = useState(false);
  const [missingModels, setMissingModels] = useState(null);

  // Critical error overlay state
  const [criticalErrors, setCriticalErrors] = useState([]);
  const [criticalErrorLogPath, setCriticalErrorLogPath] = useState('');

  // State to trigger timeline jumps
  const [timelineJump, setTimelineJump] = useState(null); // { time: number, ts: number }

  const normalizeTimestampToMs = useCallback((value, options = {}) => {
    const { assumeUtc = false } = options;
    if (value === null || value === undefined || value === '') return null;

    if (typeof value === 'number' && !Number.isNaN(value)) {
      if (value > 1e12) return value;
      if (value > 1e10) return value;
      return value * 1000;
    }

    const raw = typeof value === 'string' ? value.trim() : String(value);
    if (!raw) return null;

    const numeric = Number(raw);
    if (!Number.isNaN(numeric)) {
      if (numeric > 1e12) return numeric;
      if (numeric > 1e10) return numeric;
      return numeric * 1000;
    }

    let iso = raw;
    if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(raw)) {
      iso = raw.replace(' ', 'T');
    }
    if (assumeUtc && !/[zZ]|[+\-]\d{2}:\d{2}$/.test(iso)) {
      iso = `${iso}Z`;
    }
    const parsed = new Date(iso);
    if (!Number.isNaN(parsed.getTime())) return parsed.getTime();

    return null;
  }, []);

  useEffect(() => {
    if (!selectedEvent) {
      setSelectedDetails(null);
      setSelectedImageSrc(null);
      setLastError(null);
      return;
    }

    console.log("Loading details for event:", selectedEvent);
    setIsLoadingDetails(true);
    setLastError(null);
    setSelectedImageSrc(null); // Reset immediately

    let cancelled = false;

    const loadData = async () => {
      try {
        const targetId = selectedEvent.id === -1 ? null : selectedEvent.id;
        // Support both 'path' and 'image_path' field names for compatibility
        const targetPath = selectedEvent.path || selectedEvent.image_path;

        console.log("Loading with targetId:", targetId, "targetPath:", targetPath);

        // Load details and image in parallel
        const [det, img] = await Promise.all([
          getScreenshotDetails(targetId, targetPath),
          fetchImage(targetId, targetPath),
        ]);

        if (cancelled) return;

        console.log("Received details:", det);

        if (det && det.error) {
          throw new Error(det.error);
        }
        setSelectedDetails(det);

        // 仅 NL 搜索结果跳转时，若元数据时间与 DB 权威时间偏差较大则修正
        if (selectedEvent._fromNlSearch) {
          const recordCreatedAt = det?.record?.created_at;
          if (recordCreatedAt) {
            const dbTimestampMs = normalizeTimestampToMs(recordCreatedAt, { assumeUtc: true });
            if (dbTimestampMs && Math.abs((selectedEvent.timestamp || 0) - dbTimestampMs) > 5000) {
              setTimelineJump({ time: dbTimestampMs, ts: Date.now() });
            }
          }
        }

        console.log("Received image:", img ? "base64 data received" : "null");

        if (!img) {
          console.warn("Image fetch returned null for ID:", selectedEvent.id);
        }
        setSelectedImageSrc(img);
        setIsLoadingDetails(false);
      } catch (err) {
        if (cancelled) return;
        console.error("Failed to load details", err);
        setLastError(err.message || "Failed to load image details");
        setIsLoadingDetails(false);
      }
    };

    loadData();

    return () => {
      cancelled = true;
    };
  }, [selectedEvent]);

  // Construct boxes for InspectorOverlay from OCR results
  // PaddleOCR returns box as [[x1,y1], [x2,y2], [x3,y3], [x4,y4]] (四个角点)
  // 需要计算包围盒的最小/最大 x、y 值
  const ocrBoxes = (selectedDetails?.ocr_results || []).map((item, index) => {
    const points = item.box_coords || item.box; // 兼容两种字段名
    if (!points || !Array.isArray(points) || points.length === 0) {
      return null;
    }
    const xs = points.map(p => p[0]);
    const ys = points.map(p => p[1]);
    const minX = Math.min(...xs);
    const maxX = Math.max(...xs);
    const minY = Math.min(...ys);
    const maxY = Math.max(...ys);

    return {
      id: String(item.id ?? index),
      label: item.text,
      type: 'text',
      box: {
        x: minX,
        y: minY,
        width: maxX - minX,
        height: maxY - minY,
        unit: 'pixel'
      },
      isSensitive: false
    };
  }).filter(Boolean); // 过滤掉 null 值

  const handleCopyText = (text) => {
    navigator.clipboard.writeText(text);
  };

  const handleHideToTray = async () => {
    await getCurrentWindow().hide();

    // Send notification
    let permissionGranted = await isPermissionGranted();
    if (!permissionGranted) {
      const permission = await requestPermission();
      permissionGranted = permission === 'granted';
    }
    if (permissionGranted) {
      sendNotification({
        title: 'Carbonpaper',
        body: '程序已最小化到系统托盘，点击托盘图标可恢复窗口'
      });
    }
  };

  const handleGlobalClick = useCallback((event) => {
    // Clear highlight when clicking outside interactive nodes that opt-out
    const target = event.target;
    if (target && target.closest && target.closest('[data-keep-selection]')) {
      return;
    }
    if (highlightedEventId !== null) {
      setHighlightedEventId(null);
    }
  }, [highlightedEventId]);

  // Notification System
  const [showNotifications, setShowNotifications] = useState(false);
  const [notifications, setNotifications] = useState([]);
  const [hiddenToastIds, setHiddenToastIds] = useState(() => new Set());

  const pushNotification = useCallback((notification) => {
    if (notification?.id) {
      setHiddenToastIds((prev) => {
        if (!prev.has(notification.id)) return prev;
        const next = new Set(prev);
        next.delete(notification.id);
        return next;
      });
    }
    setNotifications((prev) => [notification, ...prev].slice(0, 200));
  }, []);

  // Security Alert Mask
  const [securityAlert, setSecurityAlert] = useState(null);

  useEffect(() => {
    let unlisten;
    const setup = async () => {
      unlisten = await listen('security-alert', (event) => {
        const payload = event.payload || {};
        setSecurityAlert({
          code: payload.code,
          message: payload.message,
          detail: payload.detail,
        });
      });
    };
    setup();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten;
    const setup = async () => {
      unlisten = await listen('app-toast', (event) => {
        const payload = event.payload || {};
        pushNotification({
          id: payload.id || `toast-${Date.now()}-${Math.random().toString(16).slice(2)}`,
          type: payload.type || 'info',
          title: payload.title || 'CarbonPaper',
          message: payload.message || '',
          details: payload.details || '',
          timestamp: payload.timestamp || Date.now(),
        });
      });
    };
    setup();
    return () => {
      if (unlisten) unlisten();
    };
  }, [pushNotification]);

  const dismissNotification = useCallback((id) => {
    setNotifications((prev) => prev.filter((n) => n.id !== id));
    setHiddenToastIds((prev) => {
      if (!prev.has(id)) return prev;
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  }, []);

  const dismissToast = useCallback((id) => {
    setHiddenToastIds((prev) => {
      if (prev.has(id)) return prev;
      const next = new Set(prev);
      next.add(id);
      return next;
    });
  }, []);

  const handleToastClose = useCallback((id, reason = 'manual') => {
    if (reason === 'timeout') {
      dismissToast(id);
      return;
    }
    dismissNotification(id);
  }, [dismissNotification, dismissToast]);

  const clearNotifications = useCallback(() => {
    setNotifications([]);
    setHiddenToastIds(new Set());
  }, []);

  const toastNotifications = useMemo(() => {
    return notifications
      .filter((notification) => notification.showToast !== false && !hiddenToastIds.has(notification.id))
      .slice(0, 3);
  }, [hiddenToastIds, notifications]);

  useEffect(() => {
    setHiddenToastIds((prev) => {
      if (prev.size === 0) return prev;
      const currentIds = new Set(notifications.map((notification) => notification.id));
      let changed = false;
      const next = new Set();
      prev.forEach((id) => {
        if (currentIds.has(id)) {
          next.add(id);
        } else {
          changed = true;
        }
      });
      return changed ? next : prev;
    });
  }, [notifications]);

  const formatErrorDetails = useCallback((err) => {
    if (!err) return '';
    if (typeof err === 'string') return err;
    if (err instanceof Error) {
      return err.stack || err.message || String(err);
    }
    try {
      return JSON.stringify(err, null, 2);
    } catch {
      return String(err);
    }
  }, []);

  const reportBackendError = useCallback((title, message, details = '') => {
    if (!message) return;
    const dedupeKey = `${message}::${details}`;
    if (lastBackendErrorRef.current === dedupeKey) return;
    lastBackendErrorRef.current = dedupeKey;
    pushNotification({
      id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
      type: 'error',
      title,
      message,
      details,
      timestamp: Date.now()
    });
  }, [pushNotification]);

  // 检查 Windows Hello 认证状态
  const checkAuthStatus = useCallback(async () => {
    try {
      const isValid = await invoke('credential_check_session');
      setIsAuthenticated(isValid);
    } catch (err) {
      console.warn('Failed to check auth status:', err);
      setIsAuthenticated(false);
    }
  }, []);

  // 认证成功回调
  const handleAuthSuccess = useCallback(() => {
    setIsAuthenticated(true);
    setAuthError(null);
  }, []);



  // Check if extension setup wizard should be shown after auth
  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated) return;
    let cancelled = false;
    (async () => {
      try {
        const needed = await invoke('check_extension_setup_needed');
        if (!cancelled && needed) {
          setShowExtensionSetup(true);
        }
      } catch (err) {
        console.warn('Failed to check extension setup status:', err);
      }
    })();
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated]);

  // 锁定会话回调
  const handleLockSession = useCallback(() => {
    setIsAuthenticated(false);
  }, []);



  // Extension setup wizard callback
  const handleExtensionSetupComplete = useCallback(() => {
    setShowExtensionSetup(false);
  }, []);

  // Check if clustering setup wizard should be shown (after Extension wizard)
  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated || showExtensionSetup) return;
    let cancelled = false;
    (async () => {
      try {
        const needed = await invoke('check_clustering_setup_needed');
        if (!cancelled && needed) {
          setShowClusteringSetup(true);
        }
      } catch (err) {
        console.warn('Failed to check clustering setup status:', err);
      }
    })();
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated, showExtensionSetup]);

  // Check if smart cluster setup wizard should be shown (after clustering wizard)
  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated || showExtensionSetup || showClusteringSetup) return;
    let cancelled = false;
    (async () => {
      try {
        const needed = await invoke('check_smart_cluster_setup_needed');
        if (!cancelled && needed) {
          setShowSmartClusterSetup(true);
        }
      } catch (err) {
        console.warn('Failed to check smart cluster setup status:', err);
      }
    })();
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated, showExtensionSetup, showClusteringSetup]);

  const handleSmartClusterSetupComplete = useCallback((enabled) => {
    setShowSmartClusterSetup(false);
    if (enabled) {
      setActiveTab('smart-cluster');
    }
  }, []);

  // Background thumbnail warmup after auth
  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated) return;
    let cancelled = false;
    console.log('[Warmup] Starting background thumbnail warmup...');
    invoke('storage_warmup_thumbnails')
      .then((result) => {
        if (!cancelled) {
          const progress = result?.progress || {};
          if (result?.started || result?.running) {
            console.log(`[Warmup] Background thumbnail warmup running — processed: ${progress.processed ?? 0}/${progress.total ?? 0}`);
          } else {
            console.log(`[Warmup] Thumbnail warmup skipped — cached: ${Boolean(result?.cached)}`);
          }
        }
      })
      .catch((err) => console.warn('[Warmup] Thumbnail warmup failed:', err));
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated]);

  const closeClusteringSetup = useCallback(() => {
    setShowClusteringSetup(false);
  }, []);

  const handleClusteringSetupComplete = useDelayedClusteringSetupRunner({
    onClose: closeClusteringSetup,
    pushNotification,
  });

  useEffect(() => {
    checkAuthStatus();
    const interval = setInterval(checkAuthStatus, 10000);
    return () => clearInterval(interval);
  }, [checkAuthStatus]);

  // 启动时从后端读取持久化的 session timeout；如果后端不存在则尝试将 localStorage 的值迁移到后端
  useEffect(() => {
    let mounted = true;
    const syncSessionTimeout = async () => {
      try {
        const res = await invoke('credential_get_session_timeout');
        const backendTimeout = Number(res);
        if (!Number.isNaN(backendTimeout) && mounted) {
          setSessionTimeout(backendTimeout);
          try {
            localStorage.setItem('sessionTimeout', String(backendTimeout));
          } catch { }
        }
      } catch (err) {
        // 后端不可用或命令缺失：尝试从 localStorage 迁移到后端（若有）
        const saved = localStorage.getItem('sessionTimeout');
        if (saved) {
          const v = parseInt(saved, 10);
          if (!Number.isNaN(v)) {
            try {
              await invoke('credential_set_session_timeout', { timeout: v });
            } catch (e) {
              console.warn('Failed to migrate session timeout to backend', e);
            }
          }
        }
      }
    };
    syncSessionTimeout();
    return () => { mounted = false; };
  }, []);

  useEffect(() => {
    const handleAuthRequired = () => {
      setIsAuthenticated(false);
    };
    window.addEventListener('cp-auth-required', handleAuthRequired);
    return () => window.removeEventListener('cp-auth-required', handleAuthRequired);
  }, []);

  const handleStartBackend = async () => {
    setAutoStartSuppressed(false);
    setBackendError('');
    setBackendStatus('waiting');
    backendStatusRef.current = 'waiting';
    backendStartAtRef.current = Date.now();
    try {
      await invoke('start_monitor');
      // Stay in waiting until IPC is reachable.
    } catch (err) {
      setBackendStatus('offline');
      backendStatusRef.current = 'offline';
      const message = err?.message || 'Failed to start backend';
      const details = formatErrorDetails(err);
      setBackendError(message);
      setAutoStartSuppressed(true);
      reportBackendError('Python 子服务启动失败', message, details);
    }
  };

  const handlePauseMonitor = useCallback(async () => {
    try {
      await invoke('pause_monitor');
      setMonitorPaused(true);
    } catch (err) {
      console.warn('Failed to pause monitor:', err);
    }
  }, []);

  const handleResumeMonitor = useCallback(async () => {
    try {
      await invoke('resume_monitor');
      setMonitorPaused(false);
    } catch (err) {
      console.warn('Failed to resume monitor:', err);
    }
  }, []);

  const checkBackendStatus = useCallback(async () => {
    const t0 = performance.now();
    try {
      const resString = await invoke('get_monitor_status');
      const elapsed = performance.now() - t0;
      if (elapsed > 5000) {
        console.warn(`[DIAG:STATUS] get_monitor_status took ${elapsed.toFixed(0)}ms`);
      }
      let res = null;
      try {
        res = JSON.parse(resString);
      } catch {
        res = null;
      }

      if (res?.stopped) {
        setBackendStatus('offline');
        backendStatusRef.current = 'offline';
        setMonitorPaused(false);
        setBackendError('');
        lastBackendErrorRef.current = '';
        backendStartAtRef.current = null;
        return;
      }

      setBackendStatus('online');
      backendStatusRef.current = 'online';
      setMonitorPaused(!!res?.paused);
      setBackendError('');
      lastBackendErrorRef.current = '';
      backendStartAtRef.current = null;
    } catch (err) {
      const elapsed = performance.now() - t0;
      if (elapsed > 5000) {
        console.warn(`[DIAG:STATUS] get_monitor_status FAILED after ${elapsed.toFixed(0)}ms:`, err);
      }
      // When we are waiting for startup, keep waiting unless start failed explicitly.
      if (backendStatusRef.current === 'waiting') {
        const startAt = backendStartAtRef.current;
        if (startAt && Date.now() - startAt < 15000) {
          return;
        }
      }
      setBackendStatus('offline');
      backendStatusRef.current = 'offline';
      const message = err?.message || 'Backend offline';
      const details = formatErrorDetails(err);
      setBackendError(message);
      reportBackendError('Python 子服务不可用', message, details);
    }
  }, [reportBackendError]);

  useEffect(() => {
    backendStatusRef.current = backendStatus;
  }, [backendStatus]);

  useEffect(() => {
    checkBackendStatus();
    const interval = setInterval(checkBackendStatus, 3000);
    return () => clearInterval(interval);
  }, [checkBackendStatus]);

  useEffect(() => {
    let unlistenExit;
    let unlistenStopped;
    const setup = async () => {
      unlistenExit = await listen('monitor-exited', (event) => {
        const payload = event?.payload || {};
        const code = payload.code || 'unknown';
        const errMsg = payload.error ? `; ${payload.error}` : '';
        const message = `子服务已退出（code: ${code}${errMsg}）`;
        const details = formatErrorDetails(payload);
        setBackendStatus('offline');
        backendStatusRef.current = 'offline';
        setBackendError(message);
        reportBackendError('Python 子服务异常退出', message, details);
      });
      unlistenStopped = await listen('monitor-stopped', () => {
        setBackendStatus('offline');
        backendStatusRef.current = 'offline';
        setMonitorPaused(false);
        setBackendError('');
        lastBackendErrorRef.current = '';
        backendStartAtRef.current = null;
      });
    };
    setup();
    return () => {
      if (unlistenExit) {
        unlistenExit();
      }
      if (unlistenStopped) {
        unlistenStopped();
      }
    };
  }, [reportBackendError, formatErrorDetails]);

  // Listen for critical errors from Rust backend
  useEffect(() => {
    let unlisten;
    const setup = async () => {
      unlisten = await listen('critical-error', (event) => {
        const msg = event.payload?.message || event.payload || 'Unknown error';
        setCriticalErrors((prev) => [...prev, msg]);
        // Fetch log path on first error
        invoke('get_log_dir').then(setCriticalErrorLogPath).catch(() => { });
      });
    };
    setup();
    return () => { if (unlisten) unlisten(); };
  }, []);

  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    const updateState = async () => {
      setIsMaximized(await appWindow.isMaximized());
    };
    updateState();

    const unlisten = appWindow.listen('tauri://resize', updateState);

    return () => {
      unlisten.then(f => f());
    }
  }, []);

  useEffect(() => {
    if (darkMode) {
      document.documentElement.classList.add('dark');
      localStorage.setItem('theme', 'dark');
    } else {
      document.documentElement.classList.remove('dark');
      localStorage.setItem('theme', 'light');
    }
  }, [darkMode]);

  useEffect(() => {
    localStorage.setItem('autoStartMonitor', autoStartMonitor ? 'true' : 'false');
  }, [autoStartMonitor]);

  useEffect(() => {
    if (!autoStartMonitor) return;
    if (autoStartSuppressed) return;
    if (powerSavingSuppressed) return;
    if (!pythonVersion) return;
    if (!depsCheckDone) return;
    if (depsNeedUpdate || depsSyncing) return;
    if (modelsNeedDownload) return;
    if (backendStatus === 'offline' && backendStatusRef.current !== 'waiting') {
      handleStartBackend();
    }
  }, [autoStartMonitor, autoStartSuppressed, powerSavingSuppressed, backendStatus, pythonVersion, handleStartBackend, depsNeedUpdate, depsSyncing, depsCheckDone, modelsNeedDownload]);

  // Auto-start condition checks powerSavingSuppressed (updated by Rust events)

  // debug: print out python version
  const refreshPythonVersion = useCallback(async () => {
    try {
      const version = await invoke('check_python_venv');
      setPythonVersion(version);

      // Check if dependencies need syncing after an update
      if (version) {
        try {
          const result = await invoke('check_deps_freshness');
          if (result?.needs_update) {
            console.log('Deps need update, reason:', result.reason);
            setDepsNeedUpdate(true);
          } else {
            setDepsNeedUpdate(false);
          }
        } catch (err) {
          console.warn('Failed to check deps freshness:', err);
          setDepsNeedUpdate(false);
        }

        // Check if model files are complete
        try {
          const modelStatus = await invoke('check_model_files');
          // Only block startup on REQUIRED models — optional feature models
          // (e.g. smart cluster reranker) are downloaded later via their own
          // setup wizard and must not gate the auto-start path.
          const hasIncomplete = Object.values(modelStatus).some((m) => !m.complete && m.required !== false);
          if (hasIncomplete) {
            console.log('Model files incomplete:', modelStatus);
            setModelsNeedDownload(true);
            setMissingModels(modelStatus);
          } else {
            setModelsNeedDownload(false);
            setMissingModels(null);
          }
        } catch (err) {
          console.warn('Failed to check model files:', err);
          setModelsNeedDownload(false);
        }
      }
    } catch (error) {
      console.error('Error fetching Python version:', error);
    } finally {
      setDepsCheckDone(true);
    }
  }, []);

  useEffect(() => {
    refreshPythonVersion();
  }, [refreshPythonVersion]);

  // Update state
  const [updateModalVisible, setUpdateModalVisible] = useState(false);
  const [updateInfo, setUpdateInfo] = useState(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [updateDownloadProgress, setUpdateDownloadProgress] = useState(null);
  const [updateDownloadError, setUpdateDownloadError] = useState(null);

  // Startup update check — delayed 5s, silent on failure
  useEffect(() => {
    const timer = setTimeout(async () => {
      try {
        const result = await checkForUpdate();
        if (result.available) {
          const dismissedVersion = localStorage.getItem('updateDismissed');
          if (result.critical || dismissedVersion !== result.version) {
            setUpdateInfo(result);
            setUpdateModalVisible(true);
          }
        }
      } catch {
        // Network failure — silently ignore
      }
    }, 5000);
    return () => clearTimeout(timer);
  }, []);

  // Debug Update Modal
  useEffect(() => {
    const handler = (e) => {
      setUpdateInfo({
        version: '9.9.9-debug',
        body: 'This is a debug update payload.\n- It supports multiline text.\n- And lists.\n\nEnjoy testing the update modal!',
        critical: e.detail?.critical || false
      });
      setUpdateModalVisible(true);
    };
    window.addEventListener('debug-update-modal', handler);
    return () => window.removeEventListener('debug-update-modal', handler);
  }, []);

  // Debug Wizards
  useEffect(() => {
    const showExtension = () => {
      setShowClusteringSetup(false);
      setShowSmartClusterSetup(false);
      setShowExtensionSetup(true);
    };
    const showClustering = () => {
      setShowExtensionSetup(false);
      setShowSmartClusterSetup(false);
      setShowClusteringSetup(true);
    };
    const showSmartCluster = () => {
      setShowExtensionSetup(false);
      setShowClusteringSetup(false);
      setShowSmartClusterSetup(true);
    };

    window.addEventListener('debug-show-extension-wizard', showExtension);
    window.addEventListener('debug-show-clustering-wizard', showClustering);
    window.addEventListener('debug-show-smart-cluster-wizard', showSmartCluster);

    return () => {
      window.removeEventListener('debug-show-extension-wizard', showExtension);
      window.removeEventListener('debug-show-clustering-wizard', showClustering);
      window.removeEventListener('debug-show-smart-cluster-wizard', showSmartCluster);
    };
  }, []);

  const handleDownloadUpdate = async () => {
    setUpdateDownloading(true);
    setUpdateDownloadError(null);
    setUpdateDownloadProgress({ phase: 'downloading', downloaded: 0, contentLength: 0 });
    try {
      await downloadAndInstallUpdate((progress) => {
        setUpdateDownloadProgress(progress);
      });
    } catch (err) {
      setUpdateDownloadError(err.message || String(err));
      setUpdateDownloading(false);
    }
  };

  // Header handlers
  const handleSearchSelect = (res) => {
    const screenshotId = res.screenshot_id !== undefined ? res.screenshot_id : (res.metadata?.screenshot_id);
    const imagePath = res.image_path || res.metadata?.image_path;
    const timestamp = res.screenshot_created_at || res.metadata?.screenshot_created_at || res.metadata?.created_at || res.created_at || new Date().toISOString();
    const isNl = res.similarity !== undefined || res.distance !== undefined || (res.metadata?.screenshot_id !== undefined && res.screenshot_id === undefined);
    const timestampMs = normalizeTimestampToMs(timestamp, { assumeUtc: !isNl });

    if (screenshotId !== undefined || imagePath) {
      setSelectedEvent({
        id: screenshotId || -1,
        path: imagePath,
        appName: res.process_name || res.metadata?.process_name,
        windowTitle: res.window_title || res.metadata?.window_title,
        timestamp: timestampMs ?? Date.now(),
        _fromNlSearch: isNl
      });
      setHighlightedEventId(screenshotId || -1);
      if (timestampMs) {
        setTimelineJump({ time: timestampMs, ts: Date.now() });
      }
    }
    setActiveTab('preview');
  };

  const handleSearchSubmit = ({ query, mode }) => {
    setActiveTab('advanced-search');
    setSearchMode(mode);
    setAdvancedSearchParams({ query, mode, refreshKey: Date.now() });
  };

  const onMinimize = () => getCurrentWindow().minimize();
  const onToggleMaximize = () => getCurrentWindow().toggleMaximize();
  const bumpTimelineRefresh = useCallback(() => {
    setTimelineRefreshKey((prev) => prev + 1);
  }, []);

  const handleDepsSync = useCallback(async () => {
    setDepsSyncing(true);
    try {
      await invoke('sync_python_deps');
      setDepsNeedUpdate(false);
    } catch (err) {
      throw err;
    } finally {
      setDepsSyncing(false);
    }
  }, []);
  return (
    <div
      data-tauri-drag-region
      className="h-screen w-screen text-ide-text overflow-hidden font-sans topbar-acrylic flex flex-col"
      onClickCapture={handleGlobalClick}
    >
      <TopBar
        darkMode={darkMode}
        setDarkMode={setDarkMode}
        setShowSettings={setShowSettings}
        showNotifications={showNotifications}
        setShowNotifications={setShowNotifications}
        isMaximized={isMaximized}
        onMinimize={onMinimize}
        onToggleMaximize={onToggleMaximize}
        onHideToTray={handleHideToTray}
        onSearchSelect={handleSearchSelect}
        onSearchSubmit={handleSearchSubmit}
        searchMode={searchMode}
        onSearchModeChange={setSearchMode}
        backendStatus={backendStatus}
        monitorPaused={monitorPaused}
        handleStartBackend={handleStartBackend}
        handlePauseMonitor={handlePauseMonitor}
        handleResumeMonitor={handleResumeMonitor}
        backendOnline={backendStatus === 'online'}
        isAuthenticated={isAuthenticated}
      />

      <div className={`flex-1 min-h-0 flex flex-col overflow-hidden relative ${isMaximized ? '' : 'mx-[3px] mb-[3px] rounded-md'}`}>
        <Mask
          backendStatus={backendStatus}
          pythonVersion={pythonVersion}
          backendError={backendError}
          handleStartBackend={handleStartBackend}
          onRefreshPythonVersion={refreshPythonVersion}
          depsNeedUpdate={depsNeedUpdate}
          depsSyncing={depsSyncing}
          onDepsSync={handleDepsSync}
          modelsNeedDownload={modelsNeedDownload}
          missingModels={missingModels}
          onModelsDownloadComplete={() => {
            setModelsNeedDownload(false);
            setMissingModels(null);
          }}
        />

        <AuthMask
          isVisible={pythonVersion && !isAuthenticated}
          onAuthSuccess={handleAuthSuccess}
          authError={authError}
          setAuthError={setAuthError}
        />

        <SecurityAlertMask
          alert={securityAlert}
          onDismiss={() => setSecurityAlert(null)}
        />

        <ErrorWindow
          isVisible={criticalErrors.length > 0}
          errors={criticalErrors}
          logPath={criticalErrorLogPath}
          onRestart={() => invoke('restart_app').catch(() => { })}
          onExit={() => invoke('exit_app').catch(() => { })}
        />

        <StartupVacuumDialog />

        {isAuthenticated && <HmacMigrationDialog />}

        <ExtensionSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && showExtensionSetup}
          onComplete={handleExtensionSetupComplete}
        />

        <ClusteringSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && !showExtensionSetup && showClusteringSetup}
          onComplete={handleClusteringSetupComplete}
        />

        <SmartClusterSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && !showExtensionSetup && !showClusteringSetup && showSmartClusterSetup}
          onComplete={handleSmartClusterSetupComplete}
        />

        <Timeline
          onSelectEvent={(evt) => {
            setSelectedEvent(evt);
            setHighlightedEventId(evt?.id ?? null);
          }}
          onClearHighlight={() => setHighlightedEventId(null)}
          jumpTimestamp={timelineJump}
          highlightedEventId={highlightedEventId}
          refreshKey={timelineRefreshKey}
          sqlPaused={!windowFocused}
        />

        {/* Main Workspace Grid */}
        <main className="flex-1 flex flex-col md:flex-row overflow-hidden relative bg-ide-bg">
          <ActivityBar
            activeTab={activeTab}
            setActiveTab={setActiveTab}
            expanded={sidebarExpanded}
            onToggleExpand={() => setSidebarExpanded(prev => !prev)}
          />

          <MainArea
            activeTab={activeTab}
            setActiveTab={setActiveTab}
            selectedImageSrc={selectedImageSrc}
            isLoadingDetails={isLoadingDetails}
            selectedEvent={selectedEvent}
            selectedDetails={selectedDetails}
            lastError={lastError}
            ocrBoxes={ocrBoxes}
            advancedSearchParams={advancedSearchParams}
            searchMode={searchMode}
            onSearchModeChange={setSearchMode}
            backendOnline={backendStatus === 'online'}
            isAuthenticated={isAuthenticated}
            onAdvancedSelect={(res) => {
              const screenshotId = res.screenshot_id !== undefined ? res.screenshot_id : (res.metadata?.screenshot_id);
              const imagePath = res.image_path || res.metadata?.image_path;
              const timestamp = res.screenshot_created_at || res.metadata?.screenshot_created_at || res.metadata?.created_at || res.created_at || new Date().toISOString();
              const isNl = res.similarity !== undefined || res.distance !== undefined || (res.metadata?.screenshot_id !== undefined && res.screenshot_id === undefined);
              const timestampMs = normalizeTimestampToMs(timestamp, { assumeUtc: !isNl });
              if (screenshotId !== undefined || imagePath) {
                setSelectedEvent({
                  id: screenshotId || -1,
                  path: imagePath,
                  appName: res.process_name || res.metadata?.process_name,
                  windowTitle: res.window_title || res.metadata?.window_title,
                  timestamp: timestampMs ?? Date.now(),
                  _fromNlSearch: isNl
                });
                setHighlightedEventId(screenshotId || -1);
                if (timestampMs) {
                  setTimelineJump({ time: timestampMs, ts: Date.now() });
                }
              }
              setActiveTab('preview');
            }}
            onInspectorBoxClick={(box) => handleCopyText(box.label)}
            onDeleteRecord={async (id) => {
              try {
                await deleteScreenshot(id);
                setSelectedEvent(null);
                setSelectedDetails(null);
                setSelectedImageSrc(null);
                bumpTimelineRefresh();
              } catch (e) {
                console.error('Failed to delete record', e);
              }
            }}
            onDeleteNearbyRecords={async (timestamp, minutes) => {
              try {
                const ts = normalizeTimestampToMs(timestamp);
                if (ts) {
                  await deleteRecordsByTimeRange(minutes, ts);
                }
                setSelectedEvent(null);
                setSelectedDetails(null);
                setSelectedImageSrc(null);
                bumpTimelineRefresh();
              } catch (e) {
                console.error('Failed to delete nearby records', e);
              }
            }}
            onCopyText={handleCopyText}
          />
        </main>
      </div>

      <NotificationToast
        notifications={toastNotifications}
        onClose={handleToastClose}
      />
      <NotificationPanel
        notifications={notifications}
        onClear={clearNotifications}
        onDismiss={dismissNotification}
        isOpen={showNotifications}
        onClosePanel={() => setShowNotifications(false)}
      />

      <UpdateModal
        isVisible={updateModalVisible}
        updateInfo={updateInfo}
        downloading={updateDownloading}
        downloadProgress={updateDownloadProgress}
        downloadError={updateDownloadError}
        onDownload={handleDownloadUpdate}
        onLater={() => {
          setUpdateModalVisible(false);
          if (updateInfo) {
            localStorage.setItem('updateDismissed', updateInfo.version);
          }
        }}
        onClose={() => setUpdateModalVisible(false)}
      />

      <SettingsDialog
        isOpen={showSettings && isAuthenticated}
        onClose={() => {
          setShowSettings(false);
          refreshPythonVersion();
        }}
        autoStartMonitor={autoStartMonitor}
        onRecordsDeleted={bumpTimelineRefresh}
        powerSavingSuppressed={powerSavingSuppressed}
        powerSavingMode={powerSavingMode}
        onPowerSavingModeChange={setPowerSavingMode}
        onAutoStartMonitorChange={(next) => {
          setAutoStartMonitor(next);
          if (next) {
            setAutoStartSuppressed(false);
          }
        }}
        onManualStartMonitor={() => setAutoStartSuppressed(false)}
        onManualStopMonitor={() => setAutoStartSuppressed(true)}
        sessionTimeout={sessionTimeout}
        onSessionTimeoutChange={setSessionTimeout}
        isSessionValid={isAuthenticated}
        onLockSession={handleLockSession}
      />
    </div>
  );
}

export default App;
