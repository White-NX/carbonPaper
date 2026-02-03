import React from 'react';
import { Monitor, Clock } from 'lucide-react';

export default function LeftSidebar({ selectedEvent, selectedDetails }) {
  return (
    <aside className="ide-panel overflow-y-auto hidden md:block border-r border-ide-border">
      <div className="ide-header">
        <span>Details</span>
      </div>
      <div className="p-4 text-sm text-ide-muted space-y-4">
        {selectedEvent ? (
          <>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">Process</label>
              <div className="flex items-center gap-2 mt-1">
                <div className="w-8 h-8 rounded bg-blue-500/20 text-blue-500 flex items-center justify-center shrink-0">
                  <Monitor size={16} />
                </div>
                <div className="overflow-hidden">
                  <div className="font-medium truncate" title={selectedDetails?.record?.process_name || selectedEvent.appName}>
                    {selectedDetails?.record?.process_name || selectedEvent.appName}
                  </div>
                </div>
              </div>
            </div>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">Window</label>
              <div className="mt-1 text-sm break-words opacity-80 line-clamp-3 select-text" title={selectedDetails?.record?.window_title || selectedEvent.windowTitle}>
                {selectedDetails?.record?.window_title || selectedEvent.windowTitle}
              </div>
            </div>
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">Time</label>
              <div className="flex items-center gap-2 mt-1 text-sm opacity-80">
                <Clock size={14} />
                {new Date(selectedEvent.timestamp).toLocaleString()}
              </div>
            </div>
          </>
        ) : (
          <p>Select an event from the timeline to view details.</p>
        )}
      </div>
    </aside>
  );
}
