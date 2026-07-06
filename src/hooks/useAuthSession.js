import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';

export function useAuthSession() {
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const [authError, setAuthError] = useState(null);
  const [sessionTimeout, setSessionTimeout] = useState(() => {
    const saved = localStorage.getItem('sessionTimeout');
    return saved ? parseInt(saved, 10) : 900;
  });

  const checkAuthStatus = useCallback(async () => {
    try {
      const isValid = await invoke('credential_check_session');
      setIsAuthenticated(isValid);
    } catch (err) {
      console.warn('Failed to check auth status:', err);
      setIsAuthenticated(false);
    }
  }, []);

  const handleAuthSuccess = useCallback(() => {
    setIsAuthenticated(true);
    setAuthError(null);
  }, []);

  const handleLockSession = useCallback(() => {
    setIsAuthenticated(false);
  }, []);

  useEffect(() => {
    checkAuthStatus();
    const interval = setInterval(checkAuthStatus, 10000);
    return () => clearInterval(interval);
  }, [checkAuthStatus]);

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
      } catch {
        const saved = localStorage.getItem('sessionTimeout');
        if (saved) {
          const v = parseInt(saved, 10);
          if (!Number.isNaN(v)) {
            try {
              await withAuth(() => invoke('credential_set_session_timeout', { timeout: v }));
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

  return {
    isAuthenticated,
    authError,
    setAuthError,
    sessionTimeout,
    setSessionTimeout,
    handleAuthSuccess,
    handleLockSession,
  };
}
