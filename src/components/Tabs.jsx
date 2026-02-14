import React from 'react';
import { Layout, Search as SearchIcon } from 'lucide-react';

export default function Tabs({ activeTab, setActiveTab }) {
  return (
    <div className="flex items-center bg-ide-panel border-b border-ide-border shrink-0 overflow-x-auto scrollbar-hide h-10">
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
    </div>
  );
}
