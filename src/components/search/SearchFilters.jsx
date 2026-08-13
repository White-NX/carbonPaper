import React from 'react';
import { CalendarRange, Filter, Tag } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { CATEGORY_COLORS } from '../../lib/categories';
import { FilterChip, ChipPanelHeading } from './FilterChip';

/** 把 Date 转成 `datetime-local` 输入需要的本地时间字符串。 */
function toDateTimeLocal(date) {
  const pad = (value) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/**
 * 由预设档位算出起止时间。结束时间一律留空表示「直到现在」。
 * @param {'any'|'today'|'week'|'month'} preset
 */
export function resolvePresetRange(preset) {
  const now = new Date();
  switch (preset) {
    case 'today': {
      const start = new Date(now.getFullYear(), now.getMonth(), now.getDate());
      return { startDate: toDateTimeLocal(start), endDate: '' };
    }
    case 'week': {
      const start = new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000);
      return { startDate: toDateTimeLocal(start), endDate: '' };
    }
    case 'month': {
      const start = new Date(now.getTime() - 30 * 24 * 60 * 60 * 1000);
      return { startDate: toDateTimeLocal(start), endDate: '' };
    }
    default:
      return { startDate: '', endDate: '' };
  }
}

/**
 * 时间范围筛选。
 *
 */
export function TimeRangeChip({ preset, startDate, endDate, onChange }) {
  const { t } = useTranslation();

  const presetOptions = [
    { value: 'any', label: t('advancedSearch.range.any') },
    { value: 'today', label: t('advancedSearch.range.today') },
    { value: 'week', label: t('advancedSearch.range.week') },
    { value: 'month', label: t('advancedSearch.range.month') },
  ];

  const activeLabel =
    preset === 'custom'
      ? t('advancedSearch.range.custom_active')
      : presetOptions.find((option) => option.value === preset)?.label;

  const isActive = preset !== 'any';

  return (
    <FilterChip
      icon={<CalendarRange className="h-3.5 w-3.5" />}
      label={isActive ? activeLabel : t('advancedSearch.range.label')}
      active={isActive}
      panelClassName="min-w-[260px]"
    >
      {({ close }) => (
        <>
          {presetOptions.map((option) => (
            <button
              key={option.value}
              type="button"
              onClick={() => {
                const range = resolvePresetRange(option.value);
                onChange(option.value, range.startDate, range.endDate);
                close();
              }}
              className={`flex w-full items-center justify-between rounded px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-ide-hover ${
                preset === option.value ? 'bg-ide-accent/10 text-ide-accent' : 'text-ide-text'
              }`}
            >
              <span>{option.label}</span>
              {preset === option.value && <span aria-hidden="true">✓</span>}
            </button>
          ))}

          <div className="my-1.5 h-px bg-ide-border" />

          <ChipPanelHeading>{t('advancedSearch.range.custom')}</ChipPanelHeading>
          <div className="flex flex-col gap-1.5 px-1 pb-1">
            <label className="flex items-center gap-2 text-[11px] text-ide-muted">
              <span className="w-8 shrink-0">{t('advancedSearch.range.start')}</span>
              <input
                type="datetime-local"
                value={startDate}
                onChange={(event) => onChange('custom', event.target.value, endDate)}
                className="flex-1 rounded border border-ide-border bg-ide-bg px-2 py-1 text-xs text-ide-text focus:border-ide-accent focus:outline-none"
              />
            </label>
            <label className="flex items-center gap-2 text-[11px] text-ide-muted">
              <span className="w-8 shrink-0">{t('advancedSearch.range.end')}</span>
              <input
                type="datetime-local"
                value={endDate}
                onChange={(event) => onChange('custom', startDate, event.target.value)}
                className="flex-1 rounded border border-ide-border bg-ide-bg px-2 py-1 text-xs text-ide-text focus:border-ide-accent focus:outline-none"
              />
            </label>
          </div>
        </>
      )}
    </FilterChip>
  );
}

/** 捕获来源（进程）筛选。 */
export function ProcessChip({ processes, selected, onChange }) {
  const { t } = useTranslation();
  const label = selected.length > 0
    ? t('advancedSearch.processes.count', { count: selected.length })
    : t('advancedSearch.processes.all');

  const toggle = (value) => {
    onChange(selected.includes(value) ? selected.filter((item) => item !== value) : [...selected, value]);
  };

  return (
    <FilterChip
      icon={<Filter className="h-3.5 w-3.5" />}
      label={label}
      active={selected.length > 0}
      panelClassName="min-w-[240px] max-h-72 overflow-y-auto custom-scrollbar"
    >
      <ChipPanelHeading
        action={selected.length > 0 && (
          <button type="button" className="text-ide-accent hover:underline" onClick={() => onChange([])}>
            {t('advancedSearch.processes.clear')}
          </button>
        )}
      >
        {t('advancedSearch.processes.select')}
      </ChipPanelHeading>

      {processes.length === 0 && (
        <div className="px-2.5 py-3 text-xs text-ide-muted">{t('advancedSearch.processes.no_data')}</div>
      )}

      {processes.map((entry) => (
        <label
          key={entry.process_name}
          className="flex cursor-pointer items-center justify-between gap-3 rounded px-2.5 py-1.5 text-xs hover:bg-ide-hover"
        >
          <span className="flex min-w-0 items-center gap-2">
            <input
              type="checkbox"
              className="accent-ide-accent"
              checked={selected.includes(entry.process_name)}
              onChange={() => toggle(entry.process_name)}
            />
            <span className="truncate">{entry.process_name}</span>
          </span>
          <span className="shrink-0 font-mono text-[11px] tabular-nums text-ide-muted">{entry.count}</span>
        </label>
      ))}
    </FilterChip>
  );
}

/** 内容分类筛选。 */
export function CategoryChip({ categories, selected, onChange }) {
  const { t } = useTranslation();
  const label = selected.length > 0
    ? t('advancedSearch.categories.count', { count: selected.length })
    : t('advancedSearch.categories.all');

  const toggle = (value) => {
    onChange(selected.includes(value) ? selected.filter((item) => item !== value) : [...selected, value]);
  };

  return (
    <FilterChip
      icon={<Tag className="h-3.5 w-3.5" />}
      label={label}
      active={selected.length > 0}
      panelClassName="min-w-[210px] max-h-72 overflow-y-auto custom-scrollbar"
    >
      <ChipPanelHeading
        action={selected.length > 0 && (
          <button type="button" className="text-ide-accent hover:underline" onClick={() => onChange([])}>
            {t('advancedSearch.categories.clear')}
          </button>
        )}
      >
        {t('advancedSearch.categories.select')}
      </ChipPanelHeading>

      {categories.length === 0 && (
        <div className="px-2.5 py-3 text-xs text-ide-muted">{t('advancedSearch.categories.no_data')}</div>
      )}

      {categories.map((category) => (
        <label
          key={category}
          className="flex cursor-pointer items-center gap-2 rounded px-2.5 py-1.5 text-xs hover:bg-ide-hover"
        >
          <input
            type="checkbox"
            className="accent-ide-accent"
            checked={selected.includes(category)}
            onChange={() => toggle(category)}
          />
          <span
            className="h-1.5 w-1.5 shrink-0 rounded-full"
            style={{ backgroundColor: CATEGORY_COLORS[category] || '#6b7280' }}
          />
          <span className="truncate">{category}</span>
        </label>
      ))}
    </FilterChip>
  );
}
