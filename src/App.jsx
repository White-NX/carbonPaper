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
import LeftSidebar from './components/LeftSidebar';
import MainArea from './components/MainArea';
import TopBar from './components/TopBar';
import { NotificationToast, NotificationPanel } from './components/Notifications';
import { getScreenshotDetails, fetchImage, deleteScreenshot, deleteRecordsByTimeRange } from './lib/monitor_api';

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

  // State to trigger timeline jumps
  const [timelineJump, setTimelineJump] = useState(null); // { time: number, ts: number }

  const normalizeTimestampToMs = useCallback((value) => {
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
    if (!/[zZ]|[+\-]\d{2}:\d{2}$/.test(iso)) {
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
        const targetPath = selectedEvent.path;

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
    const points = item.box; // [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
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
  });

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
    try {
      const resString = await invoke('get_monitor_status');
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
    if (backendStatus === 'offline' && backendStatusRef.current !== 'waiting') {
      handleStartBackend();
    }
  }, [autoStartMonitor, autoStartSuppressed, backendStatus, pythonVersion, handleStartBackend]);

  // debug: print out python version
  const refreshPythonVersion = useCallback(async () => {
    try {
      const version = await invoke('check_python_venv');
      setPythonVersion(version);
    } catch (error) {
      console.error('Error fetching Python version:', error);
    }
  }, []);

  useEffect(() => {
    refreshPythonVersion();
  }, [refreshPythonVersion]);

  // Header handlers
  const handleSearchSelect = (res) => {
    const screenshotId = res.screenshot_id !== undefined ? res.screenshot_id : (res.metadata?.screenshot_id);
    const imagePath = res.image_path || res.metadata?.image_path;
    const timestamp = res.screenshot_created_at || res.metadata?.screenshot_created_at || res.metadata?.created_at || res.created_at || new Date().toISOString();
    const timestampMs = normalizeTimestampToMs(timestamp);

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
  return (
    <div
      className="flex flex-col h-screen w-screen bg-ide-bg text-ide-text overflow-hidden font-sans"
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

      <Mask
        backendStatus={backendStatus}
        pythonVersion={pythonVersion}
        backendError={backendError}
        handleStartBackend={handleStartBackend}
        onRefreshPythonVersion={refreshPythonVersion}
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
      <main className="flex-1 flex flex-col md:grid md:grid-cols-[250px_1fr] overflow-hidden relative">
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
            const timestampMs = normalizeTimestampToMs(timestamp);
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
      />
    </div>
  );
}

export default App;
