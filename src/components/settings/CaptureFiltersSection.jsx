import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { X, Loader2, Trash2, AlertTriangle, Clock } from 'lucide-react';

export default function CaptureFiltersSection({
  filterSettings,
  processInput,
  titleInput,
  onProcessInputChange,
  onTitleInputChange,
  onAddProcess,
  onAddTitle,
  onRemoveProcess,
  onRemoveTitle,
  onToggleProtected,
  onSave,
  filtersDirty,
  savingFilters,
  saveFiltersMessage,
  onQuickDelete,
  isDeleting,
  deleteMessage,
}) {
  const { t } = useTranslation();
  const deleteOptions = [
    { id: '5min', minutes: 5, key: '5min' },
    { id: '30min', minutes: 30, key: '30min' },
    { id: '1hour', minutes: 60, key: '1hour' },
    { id: 'today', minutes: 'today', key: 'today' },
  ].map((opt) => ({ ...opt, label: t(`settings.captureFilters.quickDelete.options.${opt.key}`) }));

  return (
    <div className="space-y-8">
      {/* Quick Delete Section */}
      <QuickDeleteSection 
        onDelete={onQuickDelete}
        isDeleting={isDeleting}
        deleteMessage={deleteMessage}
      />

      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 block">{t('settings.captureFilters.title')}</label>
        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
          <div className="space-y-4">
            <div>
              <label className="block mb-2 font-semibold text-ide-text">{t('settings.captureFilters.processes.label')}</label>
              <div className="flex flex-wrap gap-2 mb-3 min-h-[1.5rem]">
                {(filterSettings.processes || []).map((p) => (
                  <span
                    key={p}
                    className="inline-flex items-center gap-1.5 pl-2.5 pr-1.5 py-1 bg-ide-panel border border-ide-border rounded-full text-xs text-ide-text group"
                  >
                    {p}
                    <button onClick={() => onRemoveProcess(p)} className="p-0.5 rounded-full hover:bg-ide-hover text-ide-muted hover:text-red-400 transition-colors" title={t('settings.captureFilters.remove')}>
                      <X className="w-3 h-3" />
                    </button>
                  </span>
                ))}
                {(filterSettings.processes || []).length === 0 && <span className="text-xs text-ide-muted py-1 italic">{t('settings.captureFilters.empty')}</span>}
              </div>
              <div className="flex gap-2">
                <input
                  className="flex-1 bg-ide-bg border border-ide-border rounded-lg px-3 py-2 text-xs text-ide-text focus:outline-none focus:border-ide-accent focus:ring-1 focus:ring-ide-accent placeholder:text-ide-muted/50"
                  value={processInput}
                  onChange={(e) => onProcessInputChange(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ',') {
                      e.preventDefault();
                      onAddProcess();
                    }
                  }}
                  placeholder={t('settings.captureFilters.processes.placeholder')}
                />
                <button
                  onClick={onAddProcess}
                  disabled={!processInput.trim()}
                  className="px-4 py-2 bg-ide-accent hover:bg-ide-accent/90 text-white rounded-lg text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {t('settings.captureFilters.add')}
                </button>
              </div>
              <p className="text-xs text-ide-muted mt-2 ml-1">{t('settings.captureFilters.processes.hint')}</p>
            </div>

            <div className="w-full h-px bg-ide-border/50" />

            <div>
              <label className="block mb-2 font-semibold text-ide-text">{t('settings.captureFilters.titles.label')}</label>
              <div className="flex flex-wrap gap-2 mb-3 min-h-[1.5rem]">
                {(filterSettings.titles || []).map((title) => (
                  <span
                    key={title}
                    className="inline-flex items-center gap-1.5 pl-2.5 pr-1.5 py-1 bg-ide-panel border border-ide-border rounded-full text-xs text-ide-text group"
                  >
                    {title}
                    <button onClick={() => onRemoveTitle(title)} className="p-0.5 rounded-full hover:bg-ide-hover text-ide-muted hover:text-red-400 transition-colors" title={t('settings.captureFilters.remove')}>
                      <X className="w-3 h-3" />
                    </button>
                  </span>
                ))}
                {(filterSettings.titles || []).length === 0 && <span className="text-xs text-ide-muted py-1 italic">{t('settings.captureFilters.empty')}</span>}
              </div>
              <div className="flex gap-2">
                <input
                  className="flex-1 bg-ide-bg border border-ide-border rounded-lg px-3 py-2 text-xs text-ide-text focus:outline-none focus:border-ide-accent focus:ring-1 focus:ring-ide-accent placeholder:text-ide-muted/50"
                  value={titleInput}
                  onChange={(e) => onTitleInputChange(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ',') {
                      e.preventDefault();
                      onAddTitle();
                    }
                  }}
                  placeholder={t('settings.captureFilters.titles.placeholder')}
                />
                <button
                  onClick={onAddTitle}
                  disabled={!titleInput.trim()}
                  className="px-4 py-2 bg-ide-accent hover:bg-ide-accent/90 text-white rounded-lg text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {t('settings.captureFilters.add')}
                </button>
              </div>
              <p className="text-xs text-ide-muted mt-2 ml-1">{t('settings.captureFilters.titles.hint')}</p>
            </div>
          </div>

          <div className="w-full h-px bg-ide-border/50" />

          <div className="flex items-center justify-between gap-4">
            <div>
              <label className="block mb-1 font-semibold text-ide-text">{t('settings.captureFilters.ignoreProtected.label')}</label>
              <p className="text-xs text-ide-muted">{t('settings.captureFilters.ignoreProtected.description')}</p>
            </div>
            <button
              onClick={onToggleProtected}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${
                filterSettings.ignoreProtected ? 'bg-ide-accent' : 'bg-ide-border'
              }`}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                  filterSettings.ignoreProtected ? 'translate-x-5' : 'translate-x-0.5'
                }`}
              />
            </button>
          </div>

          <div className="flex items-center justify-between gap-3 pt-2">
            <div className="text-xs text-ide-muted">{saveFiltersMessage}</div>
            <button
              onClick={onSave}
              disabled={!filtersDirty || savingFilters}
              className="flex items-center gap-2 px-4 py-2 bg-ide-accent hover:bg-ide-accent/90 text-white rounded-lg text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed shadow-sm"
            >
              {savingFilters && <Loader2 className="w-3.5 h-3.5 animate-spin" />} 保存过滤规则
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// Quick Delete Section Component
function QuickDeleteSection({ onDelete, isDeleting, deleteMessage }) {
  const { t } = useTranslation();
  const [showConfirm, setShowConfirm] = useState(null);
  const [deletingRange, setDeletingRange] = useState(null);

  const deleteOptionsLocal = [
    { id: '5min', minutes: 5, key: '5min' },
    { id: '30min', minutes: 30, key: '30min' },
    { id: '1hour', minutes: 60, key: '1hour' },
    { id: 'today', minutes: 'today', key: 'today' },
  ].map((opt) => ({ ...opt, label: t(`settings.captureFilters.quickDelete.options.${opt.key}`) }));

  const handleDeleteClick = (option) => {
    setShowConfirm(option.id);
  };

  const handleConfirmDelete = async (option) => {
    setDeletingRange(option.id);
    try {
      await onDelete(option.minutes);
    } finally {
      setDeletingRange(null);
      setShowConfirm(null);
    }
  };

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent flex items-center gap-2 px-1">
        {t('settings.captureFilters.quickDelete.title')}
      </label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-4">
        <p className="text-xs text-ide-muted">{t('settings.captureFilters.quickDelete.description')}</p>
        
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          {deleteOptionsLocal.map((option) => (
            <div key={option.id} className="relative">
              {showConfirm === option.id ? (
                <div className="absolute inset-0 z-10 flex flex-col justify-center gap-2 p-2 bg-red-500 rounded-lg shadow-lg animate-in fade-in zoom-in duration-200">
                  <div className="flex items-center justify-center gap-1 text-xs text-white font-medium">
                    <AlertTriangle className="w-3 h-3" />
                    <span>{t('settings.captureFilters.quickDelete.confirmTitle')}</span>
                  </div>
                  <div className="flex gap-1">
                    <button
                      onClick={() => handleConfirmDelete(option)}
                      disabled={deletingRange === option.id}
                      className="flex-1 flex items-center justify-center py-1 bg-white text-red-600 hover:bg-red-50 rounded text-[10px] font-bold transition-colors disabled:opacity-80"
                    >
                      {deletingRange === option.id ? (
                        <Loader2 className="w-3 h-3 animate-spin" />
                      ) : (
                        t('settings.captureFilters.quickDelete.yes')
                      )}
                    </button>
                    <button
                      onClick={() => setShowConfirm(null)}
                      disabled={deletingRange === option.id}
                      className="flex-1 py-1 bg-red-600 text-white hover:bg-red-700 rounded text-[10px] transition-colors disabled:opacity-80 border border-red-400"
                    >
                      {t('settings.captureFilters.quickDelete.no')}
                    </button>
                  </div>
                </div>
              ) : (
                <button
                  onClick={() => handleDeleteClick(option)}
                  disabled={isDeleting}
                  className="w-full flex items-center justify-center gap-2 px-3 py-3 bg-ide-panel hover:bg-red-500/10 hover:border-red-500/30 border border-ide-border rounded-lg text-xs font-medium transition-all hover:scale-[1.02] disabled:opacity-50 disabled:hover:scale-100 h-10"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  {t('settings.captureFilters.quickDelete.button', { label: option.label })}
                </button>
              )}
            </div>
          ))}
        </div>

        {deleteMessage && (
          <div className={`text-xs flex items-center gap-2 ${deleteMessage.includes('成功') ? 'text-green-500' : 'text-red-500'}`}>
            <div className={`w-1.5 h-1.5 rounded-full ${deleteMessage.includes('成功') ? 'bg-green-500' : 'bg-red-500'}`} />
            {deleteMessage}
          </div>
        )}
      </div>
    </div>
  );
}
