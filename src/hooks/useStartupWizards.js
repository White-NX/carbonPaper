import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { useDelayedClusteringSetupRunner } from './useDelayedClusteringSetupRunner';

export function useStartupWizards({ backendStatus, isAuthenticated, setActiveTab, pushNotification }) {
  const [showExtensionSetup, setShowExtensionSetup] = useState(false);
  const [showClusteringSetup, setShowClusteringSetup] = useState(false);
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

  const handleExtensionSetupComplete = useCallback(() => {
    setShowExtensionSetup(false);
  }, []);

  const closeClusteringSetup = useCallback(() => {
    setShowClusteringSetup(false);
  }, []);

  const handleClusteringSetupComplete = useDelayedClusteringSetupRunner({
    onClose: closeClusteringSetup,
    pushNotification,
  });

  const handleSmartClusterSetupComplete = useCallback((enabled) => {
    setShowSmartClusterSetup(false);
    if (enabled) {
      setActiveTab('smart-cluster');
    }
  }, [setActiveTab]);

  return {
    showExtensionSetup,
    showClusteringSetup,
    showSmartClusterSetup,
    handleExtensionSetupComplete,
    handleClusteringSetupComplete,
    handleSmartClusterSetupComplete,
  };
}
