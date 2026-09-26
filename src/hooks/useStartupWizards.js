import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { useDebugOverlay } from '../components/overlay/coordinator';

export function useStartupWizards({ backendStatus, isAuthenticated, setActiveTab }) {
  const [showExtensionSetup, setShowExtensionSetup] = useState(false);
  const [showSmartClusterSetup, setShowSmartClusterSetup] = useState(false);

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

  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated || showExtensionSetup) return;
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
  }, [backendStatus, isAuthenticated, showExtensionSetup]);

  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated) return;
    let cancelled = false;
    withAuth(() => invoke('storage_warmup_thumbnails'))
      .then((result) => {
        if (!cancelled) {
          const progress = result?.progress || {};
          if (result?.started || result?.running) {
          } else {
          }
        }
      })
      .catch((err) => console.warn('[Warmup] Thumbnail warmup failed:', err));
    return () => { cancelled = true; };
  }, [backendStatus, isAuthenticated]);

  useDebugOverlay('extensionSetup', () => {
    setShowSmartClusterSetup(false);
    setShowExtensionSetup(true);
  });
  useDebugOverlay('smartClusterSetup', () => {
    setShowExtensionSetup(false);
    setShowSmartClusterSetup(true);
  });

  const handleExtensionSetupComplete = useCallback(() => {
    setShowExtensionSetup(false);
  }, []);

  const handleSmartClusterSetupComplete = useCallback((enabled) => {
    setShowSmartClusterSetup(false);
    if (enabled) {
      setActiveTab('smart-cluster');
    }
  }, [setActiveTab]);

  return {
    showExtensionSetup,
    showSmartClusterSetup,
    handleExtensionSetupComplete,
    handleSmartClusterSetupComplete,
  };
}
