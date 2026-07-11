import React, { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';

export default function DropdownSelect({ value, onChange, options }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);

  useEffect(() => {
    const handleClickOutside = (event) => {
      if (ref.current && !ref.current.contains(event.target)) {
        setOpen(false);
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
    return undefined;
  }, [open]);

  const selectedOption = options.find((opt) => opt.value === value) || options[0];

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex items-center gap-2 px-3 py-1.5 bg-ide-panel border border-ide-border rounded-lg text-xs text-ide-text hover:bg-ide-hover transition-colors min-w-[160px]"
      >
        <span className="flex-1 text-left">{selectedOption.label}</span>
        <ChevronDown className={`w-3.5 h-3.5 text-ide-muted transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && (
        <div className="absolute right-0 top-full mt-1.5 w-44 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden">
          {options.map((opt) => (
            <button
              type="button"
              key={opt.value}
              onClick={() => {
                setOpen(false);
                onChange(opt.value);
              }}
              className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${opt.value === value ? 'bg-ide-accent/10' : ''}`}
            >
              <span className="text-xs text-ide-text">{opt.label}</span>
              {opt.value === value && (
                <div className="w-1.5 h-1.5 rounded-full bg-ide-accent shrink-0" />
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
