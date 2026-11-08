import { useCallback, useEffect, useRef, useState } from 'react';
import { deleteRecordsByTimeRange, updateMonitorFilters } from '../../../lib/monitor_api';
import { defaultFilterSettings, normalizeList } from '../filterUtils';
import { useSettingsActivity } from '../SettingsActivityContext';
import { notifySettingsChanged } from '../../../lib/settings_api';
import { setPreference, subscribePreference } from '../../../lib/preference_store';

export function filtersFromBackend(filters) {
  return { processes: filters.processes || [], titles: filters.titles || [], ignoreProtected: filters.ignore_protected ?? true };
}

// Keep the user's additions/removals when committed rules change in the tray.
function rebaseDraft(draft, before, after) {
  const rebaseList = (key) => {
    const removed = new Set(before[key].filter((item) => !draft[key].includes(item)));
    const added = draft[key].filter((item) => !before[key].includes(item));
    return [...new Set([...after[key].filter((item) => !removed.has(item)), ...added])];
  };
  return {
    processes: rebaseList('processes'), titles: rebaseList('titles'),
    ignoreProtected: draft.ignoreProtected === before.ignoreProtected ? after.ignoreProtected : draft.ignoreProtected,
  };
}

export function readSavedCaptureFilters() {
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
  const [filterSettings, setFilterSettings] = useState(readSavedCaptureFilters);
  const [processInput, setProcessInput] = useState('');
  const [titleInput, setTitleInput] = useState('');
  const [filtersDirty, setFiltersDirty] = useState(false);
  const [savingFilters, setSavingFilters] = useState(false);
  const [saveFiltersMessage, setSaveFiltersMessage] = useState('');
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteMessage, setDeleteMessage] = useState('');
  const [deleteMessageType, setDeleteMessageType] = useState('neutral');
  const [pendingApply, setPendingApply] = useState(false);
  const filterSettingsRef = useRef(filterSettings);
  const hasDraft = filtersDirty || Boolean(processInput.trim() || titleInput.trim());
  const hasDraftRef = useRef(hasDraft);
  hasDraftRef.current = hasDraft || savingFilters || pendingApply;
  const committed = useRef(filterSettings);
  useSettingsActivity('capture-filters', { dirty: hasDraft, busy: savingFilters });

  useEffect(() => subscribePreference('monitorFilters', () => {
    const next = readSavedCaptureFilters();
    const previousCommitted = committed.current;
    committed.current = next;
    setFilterSettings((previous) => hasDraftRef.current ? rebaseDraft(previous, previousCommitted, next) : next);
  }), []);

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
  }, []);

  const handleQuickDelete = async (minutes) => {
    setIsDeleting(true);
    setDeleteMessage('');
    try {
      const result = await deleteRecordsByTimeRange(minutes);
      if (result.error) {
        setDeleteMessageType('error');
        setDeleteMessage(t('settings.delete.failure', { error: result.error }));
      } else {
        setDeleteMessageType('success');
        const count = result.deleted_count || 0;
        setDeleteMessage(t('settings.delete.success', { count }));
        onRecordsDeleted?.();
      }
    } catch (e) {
      setDeleteMessageType('error');
      setDeleteMessage(t('settings.delete.failure', { error: e?.message || e }));
    } finally {
      setIsDeleting(false);
    }
  };

  const handleSaveFilters = async () => {
    if (savingFilters) return;
    setSavingFilters(true);
    setSaveFiltersMessage('');

    const nextFilters = {
      ...filterSettings,
      processes: Array.from(new Set([...filterSettings.processes, ...normalizeList(processInput)])),
      titles: Array.from(new Set([...filterSettings.titles, ...normalizeList(titleInput)])),
    };
    const previousPreference = localStorage.getItem('monitorFilters');
    const previousCommitted = committed.current;
    try {
      setPreference('monitorFilters', JSON.stringify(nextFilters));
      filterSettingsRef.current = nextFilters;
      setFilterSettings(nextFilters);
      setProcessInput('');
      setTitleInput('');
      setFiltersDirty(false);
      const result = await syncFiltersToMonitor(nextFilters);
      setPendingApply(!result.ok && result.reason !== 'not_running');
      if (result.ok) {
        setSaveFiltersMessage(t(monitorStatus === 'running' ? 'settings.save_filters.synced' : 'settings.save_filters.saved_local_not_running'));
        await notifySettingsChanged(['filters']);
      } else {
        // The native file remains authoritative after a failed save. Keep the
        // user's changes as a visible draft instead of a stale committed cache.
        committed.current = previousCommitted;
        hasDraftRef.current = true;
        setFiltersDirty(true);
        setPreference('monitorFilters', previousPreference);
        setFilterSettings(nextFilters);
        setSaveFiltersMessage(t('settings.feedback.saveFailed', { error: result.error?.message || result.error || '' }));
      }
    } catch (error) {
      setSaveFiltersMessage(t('settings.feedback.saveFailed', { error: String(error) }));
    } finally {
      setSavingFilters(false);
    }
  };

  return {
    filterSettings,
    processInput,
    setProcessInput,
    titleInput,
    setTitleInput,
    filtersDirty: hasDraft,
    pendingApply,
    savingFilters,
    saveFiltersMessage,
    isDeleting,
    deleteMessage,
    deleteMessageType,
    addProcessTags,
    addTitleTags,
    removeProcessTag,
    removeTitleTag,
    handleToggleProtected,
    handleQuickDelete,
    handleSaveFilters,
  };
}
