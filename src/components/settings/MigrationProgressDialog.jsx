import React from 'react';
import { Dialog } from '../Dialog';
import { invoke } from '@tauri-apps/api/core';

export default function MigrationProgressDialog({ isOpen, onClose, progress, error }) {
  const percent = progress && progress.total_files > 0 ? Math.round((progress.copied_files / progress.total_files) * 100) : 0;

  return (
    <Dialog isOpen={isOpen} onClose={onClose} title="迁移 data 目录" maxWidth="max-w-xl">
      <div className="p-4 space-y-3">
        {error ? (
          <div className="text-sm text-red-400">错误: {error}</div>
        ) : (
          <>
            <div className="text-sm text-ide-muted">正在拷贝文件: {progress.copied_files}/{progress.total_files}</div>
            <div className="w-full bg-ide-bg border border-ide-border rounded overflow-hidden h-3">
              <div className="bg-ide-accent h-3" style={{ width: `${percent}%` }} />
            </div>
            <div className="text-xs text-ide-muted">当前文件: {progress.current_file || '--'}</div>
          </>
        )}

        <div className="flex items-center justify-end gap-2 pt-3">
          {!error && (
            <button
              className="px-3 py-1 text-sm rounded border border-ide-border bg-ide-panel"
              onClick={async () => {
                try {
                  await invoke('storage_migration_cancel');
                } catch (e) {
                  // ignore if not implemented
                }
              }}
            >取消</button>
          )}
        </div>
      </div>
    </Dialog>
  );
}
