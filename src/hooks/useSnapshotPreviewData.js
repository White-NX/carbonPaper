import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';
import { getSnapshotPreviewKey } from '../lib/snapshot_preview';

const empty = { key: null, imageSrc: null, details: null, loading: false, error: null };

/** Independent image/detail completion, with no retained image data after lock or close. */
export function useSnapshotPreviewData(item, enabled = true) {
  const key = getSnapshotPreviewKey(item);
  const id = item?.screenshot_id ?? item?.id ?? item?.metadata?.screenshot_id;
  const targetId = typeof id === 'number' && id > 0 ? id : null;
  const targetPath = item?.image_path || item?.path || item?.metadata?.image_path || null;
  const [state, setState] = useState(empty);
  const [revision, setRevision] = useState(0);
  const generation = useRef(0);
  const retry = useCallback(() => setRevision((value) => value + 1), []);

  useEffect(() => {
    const request = ++generation.current;
    if (!enabled || !key) { setState(empty); return undefined; }
    let live = true;
    setState({ ...empty, key, loading: true });
    const update = (change) => {
      if (live && generation.current === request) setState((previous) => ({ ...previous, ...change }));
    };
    const loadImage = async () => {
      try { update({ imageSrc: await fetchImage(targetId, targetPath) }); }
      catch (error) { update({ error: error?.message || String(error) }); }
    };
    const loadDetails = async () => {
      try {
        const details = await getScreenshotDetails(targetId, targetPath);
        if (details?.error) throw new Error(details.error);
        update({ details });
      } catch (error) { update({ error: error?.message || String(error) }); }
    };
    void Promise.all([loadImage(), loadDetails()]).then(() => update({ loading: false }));
    return () => { live = false; };
  }, [enabled, key, targetId, targetPath, revision]);

  useEffect(() => {
    if (!enabled || !targetId || !key) return undefined;
    let live = true;
    let unlisten;
    const request = generation.current;
    listen('office-document-ref-updated', async ({ payload }) => {
      if (payload?.screenshot_id !== targetId) return;
      try {
        const details = await getScreenshotDetails(targetId, targetPath);
        if (live && generation.current === request && details && !details.error) setState((previous) => ({ ...previous, details }));
      } catch { /* The next explicit retry can recover a missing reference. */ }
    }).then((dispose) => { if (live) unlisten = dispose; else dispose(); });
    return () => { live = false; unlisten?.(); };
  }, [enabled, key, targetId, targetPath, revision]);

  return { ...(enabled && state.key === key ? state : empty), retry };
}
