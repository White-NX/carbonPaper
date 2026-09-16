import React, { useId } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, X } from 'lucide-react';
import { SettingsButton, SettingsSwitch } from './SettingsControls';
import { SettingsDivider, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus } from './SettingsPrimitives';

export default function CaptureFiltersSection({
  filterSettings, processInput, titleInput, onProcessInputChange, onTitleInputChange,
  onAddProcess, onAddTitle, onRemoveProcess, onRemoveTitle, onToggleProtected,
  onSave, filtersDirty, savingFilters, saveFiltersMessage, pendingApply,
}) {
  const { t } = useTranslation();
  const id = useId();
  const lists = [
    { key: 'processes', input: processInput, change: onProcessInputChange, add: onAddProcess, remove: onRemoveProcess },
    { key: 'titles', input: titleInput, change: onTitleInputChange, add: onAddTitle, remove: onRemoveTitle },
  ];
  return (
    <SettingsSection id="capture-rules" title={t('settings.captureFilters.title')}>
      <SettingsGroup>
        {lists.map((list, index) => (
          <React.Fragment key={list.key}>
            {index > 0 && <SettingsDivider />}
            <div>
              <label htmlFor={id + list.key} className="block text-sm font-medium">{t(`settings.captureFilters.${list.key}.label`)}</label>
              <p className="mt-1 text-xs leading-relaxed text-ide-muted">{t(`settings.captureFilters.${list.key}.hint`)}</p>
              <div className="my-3 flex flex-wrap gap-2">
                {(filterSettings[list.key] || []).map((item) => (
                  <span key={item} className="inline-flex max-w-full items-center gap-1.5 rounded-full border border-ide-border bg-ide-panel py-1 pl-2.5 pr-1.5 text-xs">
                    <span className="break-all">{item}</span>
                    <button type="button" disabled={savingFilters} onClick={() => list.remove(item)}
                      aria-label={t('settings.captureFilters.remove') + ' ' + item} className="shrink-0 rounded-full p-0.5 text-ide-muted hover:text-ide-error"><X className="h-3 w-3" /></button>
                  </span>
                ))}
                {!filterSettings[list.key]?.length && <span className="text-xs text-ide-muted">{t('settings.captureFilters.empty')}</span>}
              </div>
              <div className="flex gap-2">
                <input id={id + list.key} value={list.input} disabled={savingFilters}
                  onChange={(event) => list.change(event.target.value)}
                  onKeyDown={(event) => { if (!event.nativeEvent.isComposing && (event.key === 'Enter' || event.key === ',')) { event.preventDefault(); list.add(); } }}
                  placeholder={t(`settings.captureFilters.${list.key}.placeholder`)}
                  className="min-w-0 flex-1 rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-sm placeholder:text-ide-muted" />
                <SettingsButton onClick={list.add} disabled={savingFilters || !list.input.trim()}>{t('settings.captureFilters.add')}</SettingsButton>
              </div>
            </div>
          </React.Fragment>
        ))}
        <SettingsDivider />
        <SettingsRow label={t('settings.captureFilters.ignoreProtected.label')} description={t('settings.captureFilters.ignoreProtected.description')}
          control={<SettingsSwitch checked={filterSettings.ignoreProtected} onChange={onToggleProtected} disabled={savingFilters} />} />
        <SettingsDivider />
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0 flex-1">{saveFiltersMessage && <SettingsStatus tone={pendingApply ? 'warning' : 'neutral'}>{saveFiltersMessage}</SettingsStatus>}</div>
          <SettingsButton variant="primary" disabled={savingFilters || (!filtersDirty && !pendingApply)} onClick={onSave}
            icon={savingFilters ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : undefined}>
            {t(pendingApply && !filtersDirty ? 'settings.feedback.retryApply' : 'settings.captureFilters.save')}
          </SettingsButton>
        </div>
      </SettingsGroup>
    </SettingsSection>
  );
}
