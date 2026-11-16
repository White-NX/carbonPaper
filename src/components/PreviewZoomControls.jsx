import React, { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { Check, ChevronDown, Maximize, ZoomIn, ZoomOut } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from '../lib/utils';
import { Button, focusStyle } from './ui/Button';
import { MenuItem, MenuPanel } from './ui/Menu';

const ZOOM_PRESETS = [20, 25, 50, 75, 100, 125, 150, 200, 300, 400, 500, 1000];

export default function PreviewZoomControls({ scale, onZoomIn, onZoomOut, onScaleChange, onReset, viewportRef }) {
  const { t } = useTranslation();
  const [menuOpen, setMenuOpen] = useState(false);
  const selectorRef = useRef(null);
  const menuId = useId();
  const chooseLabel = t('snapshotPreview.chooseZoom');

  useEffect(() => {
    if (!menuOpen) return undefined;
    const outside = (event) => {
      if (!selectorRef.current?.contains(event.target)) setMenuOpen(false);
    };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [menuOpen]);

  useLayoutEffect(() => {
    if (!menuOpen) return undefined;
    const menu = selectorRef.current?.querySelector('[role="menu"]');
    const trigger = selectorRef.current?.querySelector('button');
    const viewport = viewportRef.current;
    if (!menu || !trigger || !viewport) return undefined;
    const resize = () => {
      const availableHeight = viewport.getBoundingClientRect().bottom - trigger.getBoundingClientRect().bottom - 16;
      menu.style.maxHeight = `${Math.max(0, Math.min(256, availableHeight))}px`;
    };
    resize();
    (menu.querySelector('[aria-checked="true"]') || menu.querySelector('button'))?.focus();
    const observer = new ResizeObserver(resize);
    observer.observe(viewport);
    return () => observer.disconnect();
  }, [menuOpen, viewportRef]);

  const closeMenu = () => {
    selectorRef.current?.querySelector('button')?.focus();
    setMenuOpen(false);
  };

  const handleMenuKeyDown = (event) => {
    if (event.key === 'Escape' && menuOpen) {
      event.preventDefault();
      event.stopPropagation();
      closeMenu();
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      if (!menuOpen) {
        setMenuOpen(true);
        return;
      }
      const options = [...selectorRef.current.querySelectorAll('[role="menuitemradio"]')];
      const index = options.indexOf(document.activeElement);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
        : (index + (event.key === 'ArrowDown' ? 1 : -1) + options.length) % options.length;
      options[next]?.focus();
    }
  };

  return (
    <div
      data-preview-zoom-controls
      className="absolute top-4 right-4 z-20 flex items-center gap-1 rounded-full border border-ide-border px-2 py-1.5 preview-action-bar"
      onPointerDown={(event) => event.stopPropagation()}
      // Rapid button clicks must not reach the image's double-click reset.
      onDoubleClick={(event) => event.stopPropagation()}
    >
      <Button variant="ghost" size="xs" onClick={onZoomOut} className="rounded-full border-0 p-1.5"
        title={t('snapshotPreview.zoomOut')} aria-label={t('snapshotPreview.zoomOut')}>
        <ZoomOut size={16} aria-hidden="true" />
      </Button>
      <div ref={selectorRef} className="relative" onKeyDown={handleMenuKeyDown}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) setMenuOpen(false);
        }}>
        <Button variant="ghost" size="xs" onClick={() => setMenuOpen((previous) => !previous)}
          className={cn('gap-1 rounded-full border-0 px-2 py-1 font-mono text-[11px] tabular-nums', menuOpen && 'bg-ide-hover text-ide-text')}
          title={chooseLabel} aria-label={`${chooseLabel}: ${Math.round(scale * 100)}%`}
          aria-haspopup="menu" aria-expanded={menuOpen} aria-controls={menuOpen ? menuId : undefined}>
          {Math.round(scale * 100)}%
          <ChevronDown className="h-3 w-3" aria-hidden="true" />
        </Button>
        {menuOpen && (
          <MenuPanel id={menuId} role="menu" aria-label={chooseLabel}
            className="right-0 mt-2 flex min-w-[112px] flex-col overflow-hidden rounded-xl p-1.5 preview-action-bar">
            <div className="preview-zoom-options min-h-0 overflow-x-hidden overflow-y-auto overscroll-contain rounded-md">
              {ZOOM_PRESETS.map((percent) => {
                const selected = Math.abs(scale - percent / 100) < 0.0001;
                return (
                  <MenuItem key={percent} role="menuitemradio" aria-checked={selected} tabIndex={-1}
                    className={cn('justify-between rounded-lg font-mono tabular-nums focus-visible:bg-ide-hover focus-visible:text-ide-text', focusStyle,
                      selected && 'bg-ide-accent/10 text-ide-accent hover:bg-ide-accent/15 hover:text-ide-accent')}
                    onClick={() => { onScaleChange(percent / 100); closeMenu(); }}>
                    <span>{percent}%</span>
                    <Check className={cn('h-3 w-3 shrink-0', !selected && 'invisible')} aria-hidden="true" />
                  </MenuItem>
                );
              })}
            </div>
          </MenuPanel>
        )}
      </div>
      <Button variant="ghost" size="xs" onClick={onZoomIn} className="rounded-full border-0 p-1.5"
        title={t('snapshotPreview.zoomIn')} aria-label={t('snapshotPreview.zoomIn')}>
        <ZoomIn size={16} aria-hidden="true" />
      </Button>
      <div className="mx-0.5 h-5 w-px bg-ide-border/50" />
      <Button variant="ghost" size="xs" onClick={onReset} className="rounded-full border-0 p-1.5"
        title={t('snapshotPreview.fitToWindow')} aria-label={t('snapshotPreview.fitToWindow')}>
        <Maximize size={16} aria-hidden="true" />
      </Button>
    </div>
  );
}
