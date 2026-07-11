import { useCallback, useEffect, useRef, useState } from 'react';
import { deleteRecordsByTimeRange, updateMonitorFilters } from '../../../lib/monitor_api';
import { defaultFilterSettings, normalizeList } from '../filterUtils';

function readInitialFilterSettings() {
  try {
    const saved = JSON.parse(localStorage.getItem('monitorFilters') || 'null');
    if (saved && typeof saved === 'object') {
      return {
        ...defaultFilterSettings,
        ...saved,
        processes: Array.isArray(saved.processes) ? saved.processes : [],
        titles: Array.isArray(saved.titles) ? saved.titles : [],
        ignoreProtected: typeof saved.ignoreProtected === 'boolean' ? saved.ignoreProtected : true,
      };
    }
  } catch (e) {
    console.warn('Failed to read saved filters', e);
  }
  return defaultFilterSettings;
}

export function useCaptureFilterSettings({
  monitorStatus,
  onRecordsDeleted,
  t,
}) {
  const [filterSettings, setFilterSettings] = useState(readInitialFilterSettings);
  const [processInput, setProcessInput] = useState('');
  const [titleInput, setTitleInput] = useState('');
  const [filtersDirty, setFiltersDirty] = useState(false);
  const [savingFilters, setSavingFilters] = useState(false);
  const [saveFiltersMessage, setSaveFiltersMessage] = useState('');
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteMessage, setDeleteMessage] = useState('');
  const filterSettingsRef = useRef(filterSettings);

  const addProcessTags = () => {
    const items = normalizeList(processInput);
    if (!items.length) return;
    setFilterSettings((prev) => {
      const merged = Array.from(new Set([...(prev.processes || []), ...items]));
      return { ...prev, processes: merged };
    });
    setProcessInput('');
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const addTitleTags = () => {
    const items = normalizeList(titleInput);
    if (!items.length) return;
    setFilterSettings((prev) => {
      const merged = Array.from(new Set([...(prev.titles || []), ...items]));
      return { ...prev, titles: merged };
    });
    setTitleInput('');
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const removeProcessTag = (tag) => {
    setFilterSettings((prev) => ({
      ...prev,
      processes: (prev.processes || []).filter((p) => p !== tag),
    }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const removeTitleTag = (tag) => {
    setFilterSettings((prev) => ({
      ...prev,
      titles: (prev.titles || []).filter((item) => item !== tag),
    }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const handleToggleProtected = () => {
    setFilterSettings((prev) => ({ ...prev, ignoreProtected: !prev.ignoreProtected }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const syncFiltersToMonitor = useCallback(async (filtersPayload = filterSettingsRef.current) => {
    if (monitorStatus !== 'running') {
      return { ok: false, reason: 'not_running' };
    }
    try {
      await updateMonitorFilters({
        processes: filtersPayload.processes,
        titles: filtersPayload.titles,
        ignore_protected: filtersPayload.ignoreProtected,
      });
      return { ok: true };
    } catch (e) {
      if (e?.code === 'unsupported') {
        return { ok: false, reason: 'unsupported' };
      }
      return { ok: false, reason: 'error', error: e };
    }
  }, [monitorStatus]);

  const handleQuickDelete = async (minutes) => {
    setIsDeleting(true);
    setDeleteMessage('');
    try {
      const result = await deleteRecordsByTimeRange(minutes);
      if (result.error) {
        setDeleteMessage(t('settings.delete.failure', { error: result.error }));
      } else {
        const count = result.deleted_count || 0;
        setDeleteMessage(t('settings.delete.success', { count }));
        onRecordsDeleted?.();
      }
    } catch (e) {
      setDeleteMessage(t('settings.delete.failure', { error: e?.message || e }));
    } finally {
      setIsDeleting(false);
    }
  };

  const handleSaveFilters = async () => {
    setSavingFilters(true);
    setSaveFiltersMessage('');

    const nextFilters = { ...filterSettings };

    setFilterSettings(nextFilters);
    setFiltersDirty(false);

    const result = await syncFiltersToMonitor(nextFilters);
    setSavingFilters(false);
    if (result.ok) {
      setSaveFiltersMessage(t('settings.save_filters.synced'));
    } else if (result.reason === 'not_running') {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_not_running'));
    } else if (result.reason === 'unsupported') {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_unsupported'));
    } else {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_sync_failed', { error: result.error?.message || result.error || 'Unknown error' }));
    }
  };

  useEffect(() => {
    filterSettingsRef.current = filterSettings;
    localStorage.setItem('monitorFilters', JSON.stringify(filterSettings));
  }, [filterSettings]);

  useEffect(() => {
    if (monitorStatus === 'running') {
      syncFiltersToMonitor();
    }
  }, [monitorStatus, syncFiltersToMonitor]);

  return {
    filterSettings,
    processInput,
    setProcessInput,
    titleInput,
    setTitleInput,
    filtersDirty,
    savingFilters,
    saveFiltersMessage,
    isDeleting,
    deleteMessage,
    addProcessTags,
    addTitleTags,
    removeProcessTag,
    removeTitleTag,
    handleToggleProtected,
    handleQuickDelete,
    handleSaveFilters,
  };
}
