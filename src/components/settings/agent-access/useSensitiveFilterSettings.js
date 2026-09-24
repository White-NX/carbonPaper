import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import {
  DEFAULT_CONTENT_FILTER_MODE,
  DEFAULT_PII_ENTITIES,
  PII_ENTITY_TYPES,
} from './agentAccessConstants';

const ALL_CATEGORIES = {
  cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true,
};

function toPayload(settings) {
  return {
    enabled: settings.filterEnabled,
    categories: settings.filterCategories,
    mode: settings.filterMode,
    pii_enabled: settings.piiEnabled,
    pii_entities: PII_ENTITY_TYPES.filter((type) => settings.piiEntities[type]),
    pii_mask_long_numbers: settings.piiMaskLongNumbers,
  };
}

export function useSensitiveFilterSettings() {
  const [filterEnabled, setFilterEnabled] = useState(true);
  const [filterCategories, setFilterCategories] = useState(ALL_CATEGORIES);
  const [filterMode, setFilterMode] = useState(DEFAULT_CONTENT_FILTER_MODE);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [piiEnabled, setPiiEnabled] = useState(true);
  const [piiEntities, setPiiEntities] = useState(DEFAULT_PII_ENTITIES);
  const [piiMaskLongNumbers, setPiiMaskLongNumbers] = useState(false);
  const [showPiiAdvanced, setShowPiiAdvanced] = useState(false);

  const current = {
    filterEnabled, filterCategories, filterMode, piiEnabled, piiEntities, piiMaskLongNumbers,
  };

  const apply = (settings) => {
    setFilterEnabled(settings.filterEnabled);
    setFilterCategories(settings.filterCategories);
    setFilterMode(settings.filterMode);
    setPiiEnabled(settings.piiEnabled);
    setPiiEntities(settings.piiEntities);
    setPiiMaskLongNumbers(settings.piiMaskLongNumbers);
  };

  // Shows the change at once and restores the previous settings if saving fails.
  const save = async (changes) => {
    const previous = current;
    const next = { ...current, ...changes };
    apply(next);
    try {
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: toPayload(next),
      }), { autoPrompt: true });
    } catch (e) {
      apply(previous);
      console.error('Failed to save filter config:', e);
    }
  };

  const loadFilterConfig = useCallback(async () => {
    try {
      const config = await withAuth(() => invoke('mcp_get_sensitive_filter_config'));
      setFilterEnabled(config.enabled);
      setFilterCategories(config.categories);
      if (config.mode) setFilterMode(config.mode);
      if (config.pii_enabled !== undefined) setPiiEnabled(config.pii_enabled);
      if (Array.isArray(config.pii_entities)) {
        const selected = new Set(config.pii_entities);
        setPiiEntities(Object.fromEntries(PII_ENTITY_TYPES.map((type) => [type, selected.has(type)])));
      }
      setPiiMaskLongNumbers(Boolean(config.pii_mask_long_numbers));
    } catch (e) {
      console.error('Failed to load filter config:', e);
    }
  }, []);

  const filterLevel = (() => {
    if (!filterEnabled) return 'off';
    const { cat_01, cat_02, cat_03, cat_04, cat_05 } = filterCategories;
    if (cat_01 && cat_02 && cat_03 && cat_04 && cat_05) return 'standard';
    if (cat_02 && cat_05 && !cat_01 && !cat_03 && !cat_04) return 'minimal';
    return 'custom';
  })();

  const handleLevelChange = (level) => {
    if (level === 'standard') {
      return save({ filterEnabled: true, filterCategories: ALL_CATEGORIES });
    }
    if (level === 'minimal') {
      return save({
        filterEnabled: true,
        filterCategories: { cat_01: false, cat_02: true, cat_03: false, cat_04: false, cat_05: true },
      });
    }
    if (level === 'off') return save({ filterEnabled: false });
    return undefined;
  };

  const handleCategoryToggle = (category) => save({
    filterCategories: { ...filterCategories, [category]: !filterCategories[category] },
  });

  const handleFilterModeChange = (mode) => save({ filterMode: mode });

  const handlePiiToggle = () => save({ piiEnabled: !piiEnabled });

  const handlePiiEntityToggle = (type) => save({
    piiEntities: { ...piiEntities, [type]: !piiEntities[type] },
  });

  const handlePiiMaskLongNumbersToggle = () => save({ piiMaskLongNumbers: !piiMaskLongNumbers });

  return {
    filterEnabled,
    filterCategories,
    filterMode,
    showAdvanced,
    setShowAdvanced,
    piiEnabled,
    piiEntities,
    piiMaskLongNumbers,
    showPiiAdvanced,
    setShowPiiAdvanced,
    filterLevel,
    loadFilterConfig,
    handleLevelChange,
    handleCategoryToggle,
    handleFilterModeChange,
    handlePiiToggle,
    handlePiiEntityToggle,
    handlePiiMaskLongNumbersToggle,
  };
}
