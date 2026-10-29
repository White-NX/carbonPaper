import React, { createContext, useContext, useEffect, useRef, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { fetchSearchImage } from '../../lib/monitor_api';

export const SearchImageActivityContext = createContext(true);

/** Load on first visibility; release the original when its workspace is hidden or locked. */
export function OriginalSearchImage({ item, placeholder }) {
  const { t } = useTranslation();
  const active = useContext(SearchImageActivityContext);
  const ref = useRef(null);
  const screenshotId = item?.screenshot_id ?? item?.metadata?.screenshot_id;
  const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
  const path = id ? null : item?.image_path || item?.metadata?.image_path || item?.path || null;
  const key = id ? `id:${id}` : `path:${path}`;
  const [image, setImage] = useState({ key, src: null, loading: Boolean(id || path) });
  const current = image.key === key ? image : { src: null, loading: Boolean(id || path) };

  useEffect(() => {
    setImage({ key, src: null, loading: active && Boolean(id || path) });
    if (!active) return undefined;
    if (!id && !path) return undefined;

    let disposed = false;
    let started = false;
    let controller = null;
    const load = () => {
      if (disposed || started) return;
      started = true;
      const request = new AbortController();
      controller = request;
      fetchSearchImage(id, path, { signal: request.signal })
        .then((src) => {
          if (!disposed && !request.signal.aborted) setImage({ key, src, loading: false });
        })
        .catch(() => {
          if (!disposed && !request.signal.aborted) setImage({ key, src: null, loading: false });
        });
    };

    let observer;
    if (typeof IntersectionObserver === 'undefined') {
      load();
    } else {
      observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          load();
          observer.disconnect();
        }
      }, { rootMargin: '200px' });
      observer.observe(ref.current);
    }
    return () => {
      disposed = true;
      observer?.disconnect();
      controller?.abort();
    };
  }, [active, id, path, key]);

  return (
    <div ref={ref} className="h-full w-full">
      {current.src ? (
        <img src={current.src} alt="" className="h-full w-full object-cover" loading="lazy" decoding="async" />
      ) : (
        <div className={`flex h-full w-full items-center justify-center text-xs text-ide-muted ${current.loading ? '' : 'bg-ide-bg'}`}>
          {current.loading ? <Loader2 className="h-4 w-4 animate-spin" /> : placeholder || t('advancedSearch.no_image')}
        </div>
      )}
    </div>
  );
}
