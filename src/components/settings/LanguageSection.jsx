import React from 'react';
import { useTranslation } from 'react-i18next';
import { Check } from 'lucide-react';

const LANGUAGES = [
  { value: 'zh-CN', label: '简体中文', nativeName: '简体中文' },
  { value: 'en', label: 'English', nativeName: 'English' },
];

export default function LanguageSection() {
  const { t, i18n } = useTranslation();

  const handleSelect = (lang) => {
    i18n.changeLanguage(lang);
    localStorage.setItem('language', lang);
  };

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 block">语言 / Language</label>
      <div className="grid gap-2">
        {LANGUAGES.map((lang) => {
          const isActive = i18n.language === lang.value;
          return (
            <button
              key={lang.value}
              onClick={() => handleSelect(lang.value)}
              className={`flex items-center justify-between px-4 py-3 rounded-xl border text-left transition-colors ${
                isActive
                  ? 'bg-ide-accent/10 border-ide-accent/40 text-ide-text'
                  : 'bg-ide-bg border-ide-border text-ide-text hover:bg-ide-hover'
              }`}
            >
              <span className="text-sm font-medium">{lang.nativeName}</span>
              {isActive && <Check className="w-4 h-4 text-ide-accent shrink-0" />}
            </button>
          );
        })}
      </div>
    </div>
  );
}
