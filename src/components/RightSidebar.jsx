import React from 'react';
import { Copy } from 'lucide-react';

export default function RightSidebar({ isLoadingDetails, selectedDetails, handleCopyText }) {
  return (
    <aside className="ide-panel flex flex-col border-l border-ide-border hidden md:flex h-full overflow-hidden">
      <div className="ide-header border-b border-ide-border shrink-0">
        <span>OCR Content</span>
      </div>
      <div className="flex-1 p-0 relative">
        {isLoadingDetails ? (
          <div className="p-4 space-y-3">
            {[1, 2, 3].map(i => <div key={i} className="h-4 bg-ide-panel animate-pulse rounded w-3/4"></div>)}
          </div>
        ) : (
          // 如果记录处于 pending 状态，显示斜体的占位文案
          (selectedDetails?.record?.status === 'pending') ? (
            <div className="w-full h-full flex items-center justify-center text-sm italic text-ide-muted p-4">OCR Processing…</div>
          ) : (
            <textarea
              className="w-full h-full bg-ide-bg p-4 text-xs font-mono text-ide-text resize-none focus:outline-none leading-relaxed"
              readOnly
              value={selectedDetails?.ocr_results?.map(r => r.text).join('\n') || ''}
              placeholder={selectedDetails ? "No text detected" : "Select an image to view OCR content"}
            />
          )
        )}
      </div>
      {selectedDetails?.ocr_results?.length > 0 && (
        <div className="p-2 border-t border-ide-border bg-ide-panel shrink-0 flex justify-end">
          <button
            onClick={() => {
              const text = selectedDetails?.ocr_results?.map(r => r.text).join('\n') || '';
              handleCopyText(text);
            }}
            className="flex items-center gap-2 px-3 py-1.5 bg-ide-bg hover:bg-ide-hover border border-ide-border rounded text-xs transition-colors"
          >
            <Copy size={12} /> Copy All
          </button>
        </div>
      )}
    </aside>
  );
}
