import React from 'react';
import { SettingsSelect } from '../SettingsControls';

export default function StorageOptionSelect({
  label,
  value,
  options,
  onChange,
  icon: Icon,
  description,
  className = '',
}) {
  return (
    <div className={`bg-ide-bg/70 border border-ide-border rounded-xl p-4 ${className}`}>
      <div className="flex items-center gap-3 mb-3">
        {Icon && (
          <div className="p-2 rounded-lg bg-ide-panel border border-ide-border">
            <Icon className="w-4 h-4" />
          </div>
        )}
        <div className="flex-1">
          <div className="font-medium text-sm">{label}</div>
          {description && <div className="text-xs text-ide-muted mt-0.5">{description}</div>}
        </div>
      </div>
      <SettingsSelect
        label={label}
        value={value}
        onChange={(next) => onChange(String(next))}
        options={options}
        className="w-full"
      />
    </div>
  );
}
