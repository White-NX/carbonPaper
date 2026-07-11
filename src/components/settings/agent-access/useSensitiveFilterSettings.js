import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';

export function useSensitiveFilterSettings({ t, onError }) {
  const [filterEnabled, setFilterEnabled] = useState(true);
  const [filterCategories, setFilterCategories] = useState({
    cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true,
  });
  const [filterMode, setFilterMode] = useState('reject');
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [piiEnabled, setPiiEnabled] = useState(true);
  const [piiEntities, setPiiEntities] = useState({
    PHONE_NUMBER: true, CN_ID_CARD: true,
    EMAIL_ADDRESS: true, CN_BANK_CARD: true, ADDRESS: true,
  });
  const [spacyModels, setSpacyModels] = useState({
    zh_core_web_sm: { installed: false },
    en_core_web_sm: { installed: false },
  });
  const [downloadingModel, setDownloadingModel] = useState(null);
  const [recheckLoading, setRecheckLoading] = useState(false);
  const [showPiiAdvanced, setShowPiiAdvanced] = useState(false);

  const loadFilterConfig = useCallback(async () => {
    try {
      const filterConfig = await withAuth(() => invoke('mcp_get_sensitive_filter_config'));
      setFilterEnabled(filterConfig.enabled);
      setFilterCategories(filterConfig.categories);
      if (filterConfig.mode) setFilterMode(filterConfig.mode);
      if (filterConfig.presidio_enabled !== undefined) setPiiEnabled(filterConfig.presidio_enabled);
      if (filterConfig.presidio_entities && filterConfig.presidio_entities.length > 0) {
        const entityMap = {};
        for (const entity of filterConfig.presidio_entities) entityMap[entity] = true;
        setPiiEntities((prev) => {
          const merged = { ...prev };
          for (const key of Object.keys(merged)) merged[key] = !!entityMap[key];
          return merged;
        });
      }
    } catch (e) {
      console.error('Failed to load filter config:', e);
    }
  }, []);

  const loadSpacyModels = useCallback(async () => {
    try {
      const models = await invoke('check_spacy_models');
      setSpacyModels(models);
    } catch (e) {
      console.error('Failed to check spaCy models:', e);
    }
  }, []);

  useTauriEventListener('spacy-model-status', (event) => {
    const { model, status } = event.payload;
    if (status === 'installing') {
      setDownloadingModel(model);
    } else if (status === 'installed') {
      setSpacyModels((prev) => ({ ...prev, [model]: { installed: true } }));
      setDownloadingModel((prev) => prev === model ? null : prev);
    } else if (status === 'failed') {
      setDownloadingModel((prev) => prev === model ? null : prev);
    }
  });

  const filterLevel = (() => {
    if (!filterEnabled) return 'off';
    const { cat_01, cat_02, cat_03, cat_04, cat_05 } = filterCategories;
    if (cat_01 && cat_02 && cat_03 && cat_04 && cat_05) return 'standard';
    if (cat_02 && cat_05 && !cat_01 && !cat_03 && !cat_04) return 'minimal';
    return 'custom';
  })();

  const handleLevelChange = async (level) => {
    let newEnabled = filterEnabled;
    let newCategories = { ...filterCategories };
    if (level === 'standard') {
      newEnabled = true;
      newCategories = { cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true };
    } else if (level === 'minimal') {
      newEnabled = true;
      newCategories = { cat_01: false, cat_02: true, cat_03: false, cat_04: false, cat_05: true };
    } else if (level === 'off') {
      newEnabled = false;
    }
    setFilterEnabled(newEnabled);
    setFilterCategories(newCategories);
    try {
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: newEnabled,
          categories: newCategories,
          mode: filterMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((key) => piiEntities[key]),
        },
      }), { autoPrompt: true });
    } catch (e) {
      setFilterEnabled(filterEnabled);
      setFilterCategories(filterCategories);
      console.error('Failed to save filter config:', e);
    }
  };

  const handleCategoryToggle = async (category) => {
    const newCategories = { ...filterCategories, [category]: !filterCategories[category] };
    setFilterCategories(newCategories);
    try {
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: newCategories,
          mode: filterMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((key) => piiEntities[key]),
        },
      }), { autoPrompt: true });
    } catch (e) {
      setFilterCategories(filterCategories);
      console.error('Failed to save filter config:', e);
    }
  };

  const handleFilterModeChange = async (newMode) => {
    const prevMode = filterMode;
    setFilterMode(newMode);
    try {
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: filterCategories,
          mode: newMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((key) => piiEntities[key]),
        },
      }), { autoPrompt: true });
    } catch (e) {
      setFilterMode(prevMode);
      console.error('Failed to save filter config:', e);
    }
  };

  const savePiiConfig = async (newEnabled, newEntities) => {
    try {
      const entityList = Object.keys(newEntities).filter((key) => newEntities[key]);
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: filterCategories,
          mode: filterMode,
          presidio_enabled: newEnabled,
          presidio_entities: entityList,
        },
      }), { autoPrompt: true });
    } catch (e) {
      console.error('Failed to save PII config:', e);
    }
  };

  const handlePiiToggle = async () => {
    const newVal = !piiEnabled;
    setPiiEnabled(newVal);
    await savePiiConfig(newVal, piiEntities);
  };

  const handlePiiEntityToggle = async (entityType) => {
    const newEntities = { ...piiEntities, [entityType]: !piiEntities[entityType] };
    setPiiEntities(newEntities);
    await savePiiConfig(piiEnabled, newEntities);
  };

  const handleDownloadModel = async (modelName) => {
    setDownloadingModel(modelName);
    try {
      await withAuth(
        () => invoke('install_spacy_model', { modelName }),
        { autoPrompt: true },
      );
      const models = await invoke('check_spacy_models');
      setSpacyModels(models);
    } catch (e) {
      onError?.(String(e));
    } finally {
      setDownloadingModel(null);
    }
  };

  const handleForceRecheck = async () => {
    setRecheckLoading(true);
    try {
      const models = await invoke('force_recheck_spacy_models');
      setSpacyModels(models);
    } catch (e) {
      onError?.(String(e));
    } finally {
      setRecheckLoading(false);
    }
  };

  return {
    filterEnabled,
    filterCategories,
    filterMode,
    showAdvanced,
    setShowAdvanced,
    piiEnabled,
    piiEntities,
    spacyModels,
    downloadingModel,
    recheckLoading,
    showPiiAdvanced,
    setShowPiiAdvanced,
    filterLevel,
    loadFilterConfig,
    loadSpacyModels,
    handleLevelChange,
    handleCategoryToggle,
    handleFilterModeChange,
    handlePiiToggle,
    handlePiiEntityToggle,
    handleDownloadModel,
    handleForceRecheck,
  };
}
