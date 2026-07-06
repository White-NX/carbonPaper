import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTauriEventListener } from './useTauriEventListener';

export function useAppNotifications() {
  const [showNotifications, setShowNotifications] = useState(false);
  const [notifications, setNotifications] = useState([]);
  const [hiddenToastIds, setHiddenToastIds] = useState(() => new Set());
  const [securityAlert, setSecurityAlert] = useState(null);
  const lastBackendErrorRef = useRef('');

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

  useTauriEventListener('security-alert', (event) => {
    const payload = event.payload || {};
    setSecurityAlert({
      code: payload.code,
      message: payload.message,
      detail: payload.detail,
    });
  });

  useTauriEventListener('app-toast', (event) => {
    const payload = event.payload || {};
    pushNotification({
      id: payload.id || `toast-${Date.now()}-${Math.random().toString(16).slice(2)}`,
      type: payload.type || 'info',
      title: payload.title || 'CarbonPaper',
      message: payload.message || '',
      details: payload.details || '',
      timestamp: payload.timestamp || Date.now(),
    });
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
      timestamp: Date.now(),
    });
  }, [pushNotification]);

  const resetBackendErrorDedupe = useCallback(() => {
    lastBackendErrorRef.current = '';
  }, []);

  return {
    showNotifications,
    setShowNotifications,
    notifications,
    toastNotifications,
    pushNotification,
    dismissNotification,
    handleToastClose,
    clearNotifications,
    securityAlert,
    setSecurityAlert,
    formatErrorDetails,
    reportBackendError,
    resetBackendErrorDedupe,
  };
}
