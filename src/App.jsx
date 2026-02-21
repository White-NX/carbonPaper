import React, { useState, useEffect, useRef, useCallback } from 'react';
import {
  Moon, Sun, Settings, Bell, Terminal, Layout, Minus, Square, X, Copy, Loader2, Monitor, Clock,
  WifiOff, Play, Search as SearchIcon, Info as InfoIcon, Route, PackageOpen
} from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import Timeline from './components/Timeline';
import { InspectorImage } from './components/Gallery';
import { SearchBox } from './components/SearchBox';
import { AdvancedSearch } from './components/AdvancedSearch';
import SettingsDialog from './components/settings/SettingsDialog';
import Mask from './components/Mask';
import AuthMask from './components/AuthMask';
import DmlSetupWizard from './components/DmlSetupWizard';
import LeftSidebar from './components/LeftSidebar';
import MainArea from './components/MainArea';
import TopBar from './components/TopBar';
import { NotificationToast, NotificationPanel } from './components/Notifications';
import { getScreenshotDetails, fetchImage, deleteScreenshot, deleteRecordsByTimeRange } from './lib/monitor_api';
import { checkForUpdate } from './lib/update_api';

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

  // Windows Hello Authentication State
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const [authError, setAuthError] = useState(null);
  const [showDmlSetup, setShowDmlSetup] = useState(false);
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
  const [backendError, setBackendError] = useState('');
  const backendStatusRef = useRef('unknown');
  const backendStartAtRef = useRef(null);
  const lastBackendErrorRef = useRef('');
  const [activeTab, setActiveTab] = useState('preview'); // 'preview' | 'advanced-search'
  const [searchMode, setSearchMode] = useState('ocr');
  const [advancedSearchParams, setAdvancedSearchParams] = useState({ query: '', mode: 'ocr', refreshKey: Date.now() });
  const [timelineRefreshKey, setTimelineRefreshKey] = useState(0);

  // debug
  const [pythonVersion, setPythonVersion] = useState(null);

  // Dependency sync state
  const [depsNeedUpdate, setDepsNeedUpdate] = useState(false);
  const [depsSyncing, setDepsSyncing] = useState(false);
  const [depsCheckDone, setDepsCheckDone] = useState(false);

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

    // Sequential requests to avoid pipe busy errors
    const loadData = async () => {
      try {
        const targetId = selectedEvent.id === -1 ? null : selectedEvent.id;
        // Support both 'path' and 'image_path' field names for compatibility
        const targetPath = selectedEvent.path || selectedEvent.image_path;

        console.log("Loading with targetId:", targetId, "targetPath:", targetPath);

        // First get details
        const det = await getScreenshotDetails(targetId, targetPath);
        console.log("Received details:", det);

        if (det && det.error) {
          throw new Error(det.error);
        }
        setSelectedDetails(det);

        // Then get image
        const img = await fetchImage(targetId, targetPath);
        console.log("Received image:", img ? "base64 data received" : "null");

        if (!img) {
          console.warn("Image fetch returned null for ID:", selectedEvent.id);
        }
        setSelectedImageSrc(img);
        setIsLoadingDetails(false);
      } catch (err) {
        console.error("Failed to load details", err);
        setLastError(err.message || "Failed to load image details");
        setIsLoadingDetails(false);
      }
    };

    loadData();
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
        title: 'Carbon Paper',
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

  const pushNotification = useCallback((notification) => {
    setNotifications((prev) => [notification, ...prev].slice(0, 200));
  }, []);

  const dismissNotification = useCallback((id) => {
    setNotifications((prev) => prev.filter((n) => n.id !== id));
  }, []);

  const clearNotifications = useCallback(() => {
    setNotifications([]);
  }, []);

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

  // Check if DML setup wizard should be shown after auth
  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated) return;
    let cancelled = false;
    (async () => {
      try {
        const needed = await invoke('check_dml_setup_needed');
        if (!cancelled && needed) {
          setShowDmlSetup(true);
        }
      } catch (err) {
        console.warn('Failed to check DML setup status:', err);
      }
    })();
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated]);

  // 锁定会话回调
  const handleLockSession = useCallback(() => {
    setIsAuthenticated(false);
  }, []);

  // DML setup wizard Callback
  const handleDmlSetupComplete = useCallback(async () => {
    setShowDmlSetup(false);
    // If monitor is running, restart to apply new settings
    try {
      const resString = await invoke('get_monitor_status');
      const res = JSON.parse(resString);
      if (res && !res.stopped) {
        await invoke('stop_monitor');
        await invoke('start_monitor');
      }
    } catch {
      // ignore — monitor may not be running
    }
  }, []);

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
          } catch {}
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
        setBackendError('');
        lastBackendErrorRef.current = '';
        backendStartAtRef.current = null;
        return;
      }

      setBackendStatus('online');
      backendStatusRef.current = 'online';
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
    };
    setup();
    return () => {
      if (unlistenExit) {
        unlistenExit();
      }
    };
  }, [reportBackendError, formatErrorDetails]);
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
    if (!pythonVersion) return;
    if (!depsCheckDone) return;
    if (depsNeedUpdate || depsSyncing) return;
    if (backendStatus === 'offline' && backendStatusRef.current !== 'waiting') {
      handleStartBackend();
    }
  }, [autoStartMonitor, autoStartSuppressed, backendStatus, pythonVersion, handleStartBackend, depsNeedUpdate, depsSyncing, depsCheckDone]);

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

  // Startup update check — delayed 5s, silent on failure
  useEffect(() => {
    const timer = setTimeout(async () => {
      try {
        const result = await checkForUpdate();
        if (result.available) {
          pushNotification({
            id: `update-${Date.now()}`,
            type: 'info',
            title: '发现新版本',
            message: `新版本 v${result.version} 已发布，前往 设置 > 关于 下载更新`,
            timestamp: Date.now(),
          });
        }
      } catch {
        // Network failure — silently ignore
      }
    }, 5000);
    return () => clearTimeout(timer);
  }, [pushNotification]);

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
        timestamp: timestampMs ?? Date.now()
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
      />

      <AuthMask
        isVisible={backendStatus === 'online' && pythonVersion && !isAuthenticated}
        onAuthSuccess={handleAuthSuccess}
        authError={authError}
        setAuthError={setAuthError}
      />

      <DmlSetupWizard
        isVisible={backendStatus === 'online' && isAuthenticated && showDmlSetup}
        onComplete={handleDmlSetupComplete}
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
      />

      {/* Main Workspace Grid */}
      <main className="flex-1 flex flex-col md:grid md:grid-cols-[250px_1fr] overflow-hidden relative bg-ide-bg">
        <LeftSidebar selectedEvent={selectedEvent} selectedDetails={selectedDetails} />

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
                timestamp: timestampMs ?? Date.now()
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
        notifications={notifications.slice(0, 3)}
        onClose={dismissNotification}
      />
      <NotificationPanel
        notifications={notifications}
        onClear={clearNotifications}
        onDismiss={dismissNotification}
        isOpen={showNotifications}
        onClosePanel={() => setShowNotifications(false)}
      />

      <SettingsDialog
        isOpen={showSettings}
        onClose={() => setShowSettings(false)}
        autoStartMonitor={autoStartMonitor}
        onRecordsDeleted={bumpTimelineRefresh}
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
