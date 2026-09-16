import React, { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';

const noop = () => {};
const ActivityContext = createContext({ setActivity: noop, dirty: false, busy: false, enabled: true });
export const SettingsPageContext = createContext(true);

export function SettingsActivityProvider({ children, enabled = true }) {
  const [activities, setActivities] = useState({});
  const setActivity = useCallback((id, value) => {
    setActivities((current) => {
      if (value && current[id]?.dirty === value.dirty && current[id]?.busy === value.busy) return current;
      if (!value && !current[id]) return current;
      const next = { ...current };
      if (value) next[id] = value;
      else delete next[id];
      return next;
    });
  }, []);
  const value = useMemo(() => ({
    setActivity, enabled,
    dirty: Object.values(activities).some((activity) => activity.dirty),
    busy: Object.values(activities).some((activity) => activity.busy),
  }), [activities, enabled, setActivity]);
  return <ActivityContext.Provider value={value}>{children}</ActivityContext.Provider>;
}

export function useSettingsActivity(id, { dirty = false, busy = false } = {}) {
  const { setActivity } = useContext(ActivityContext);
  useEffect(() => {
    setActivity(id, { dirty, busy });
    return () => setActivity(id, null);
  }, [id, dirty, busy, setActivity]);
}

export function useSettingsActive() {
  const { enabled } = useContext(ActivityContext);
  const pageActive = useContext(SettingsPageContext);
  return enabled && pageActive;
}

export function useSettingsWindowActivity() { return useContext(ActivityContext); }
