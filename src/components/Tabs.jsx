import React from 'react';
import { Layout, Search as SearchIcon, Info as InfoIcon, BarChart3 } from 'lucide-react';

export default function Tabs({ activeTab, setActiveTab }) {
  return (
    <div className="flex items-center bg-ide-panel border-b border-ide-border shrink-0 overflow-x-auto scrollbar-hide">
      <button
        type="button"
        className={`ide-tab flex items-center gap-2 ${activeTab === 'preview' ? 'ide-tab-active' : ''}`}
        onClick={() => setActiveTab('preview')}
      >
        <Layout className="w-4 h-4" />
        <span>Preview</span>
      </button>
      <button
        type="button"
        className={`ide-tab flex items-center gap-2 ${activeTab === 'advanced-search' ? 'ide-tab-active' : ''}`}
        onClick={() => setActiveTab('advanced-search')}
      >
        <SearchIcon className="w-4 h-4" />
        <span>Advanced Search</span>
      </button>
      <button
        type="button"
        className={`ide-tab flex items-center gap-2 ${activeTab === 'analysis' ? 'ide-tab-active' : ''}`}
        onClick={() => setActiveTab('analysis')}
      >
        <BarChart3 className="w-4 h-4" />
        <span>Analysis</span>
      </button>
      <button
        type="button"
        className={`ide-tab flex items-center gap-2 ${activeTab === 'about' ? 'ide-tab-active' : ''}`}
        onClick={() => setActiveTab('about')}
      >
        <InfoIcon className="w-4 h-4" />
        <span>About</span>
      </button>
    </div>
  );
}
