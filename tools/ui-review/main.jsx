import React from 'react';
import ReactDOM from 'react-dom/client';
import App from '../../src/App';
import SnapshotPreviewStandalone from '../../src/components/SnapshotPreviewStandalone';
import { changeAppLanguage } from '../../src/i18n';
import '../../src/index.css';
import { REVIEW_START, snapshots } from './fixture';

const params = new URLSearchParams(location.search);
localStorage.setItem('language', params.get('lang') || 'zh-CN');
localStorage.setItem('theme', params.get('theme') || 'dark');
localStorage.setItem('autoStartMonitor', 'false');
localStorage.setItem('snapshotPreviewDetailsVisible', params.get('details') === 'true' ? 'true' : 'false');
localStorage.setItem('workspaceNavigationExpanded', 'true');
localStorage.setItem('recordsTimelineCollapsed', params.get('timeline') === 'compact' ? 'true' : 'false');
localStorage.setItem('searchTimelineCollapsed', params.get('timeline') === 'compact' ? 'true' : 'false');
// Freeze the clock of this isolated fixture so the timeline and recap date stay reproducible.
const NativeDate = Date;
const now = REVIEW_START + 25 * 60000;
globalThis.Date = class extends NativeDate {
  constructor(...args) { if (args.length) super(...args); else super(now); }
  static now() { return now; }
};
await changeAppLanguage(params.get('lang') || 'zh-CN');
const standalone = params.get('page') === 'standalone';
if (standalone) localStorage.setItem('carbonpaper:snapshot-preview-window-state', JSON.stringify({ tabs: [snapshots[0]], activeKey: 'id:1' }));
ReactDOM.createRoot(document.getElementById('root')).render(standalone ? <SnapshotPreviewStandalone /> : <App />);

const page = params.get('page');
const pageIds = { search: 'advanced-search', recap: 'recap', topics: 'smart-cluster', preview: 'advanced-search' };
if (pageIds[page]) {
  const timer = setInterval(() => {
    const buttons = [...document.querySelectorAll('nav button')];
    const index = ['preview', 'advanced-search', 'recap', 'smart-cluster'].indexOf(pageIds[page]);
    if (!buttons[index] || buttons[index].closest('[inert]')) return;
    buttons[index].click(); clearInterval(timer);
    if (page === 'preview') {
      const next = setInterval(() => {
        const snapshotButton = document.querySelector('img')?.closest('button');
        if (snapshotButton && !snapshotButton.closest('nav')) { snapshotButton.click(); clearInterval(next); }
      }, 150);
    }
  }, 100);
}
