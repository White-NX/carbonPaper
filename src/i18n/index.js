import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import LanguageDetector from 'i18next-browser-languagedetector';
import { invoke } from '@tauri-apps/api/core';

// Dynamic import loader: lazy-load locale JSON via ESM import()
const loadLocale = async (lng) => {
  try {
    // existing files are at ./locales/{lng}.json (e.g. zh-CN.json, en.json)
    const mod = await import(`./locales/${lng}.json`);
    return mod.default || mod;
  } catch (e) {
    console.warn(`i18n: failed to load locale ${lng}`, e);
    return null;
  }
};

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    ns: ['translation'],
    defaultNS: 'translation',
    fallbackLng: 'zh-CN',
    debug: false,
    interpolation: {
      escapeValue: false,
    },
    react: {
      useSuspense: true,
      bindI18nStore: 'added',
    },
  });

// ensure the detected/selected language has resources loaded
const ensureLoaded = async (lng) => {
  const existing = i18n.hasResourceBundle(lng, 'translation');
  if (!existing) {
    const data = await loadLocale(lng);
    if (data) {
      i18n.addResourceBundle(lng, 'translation', data, true, true);
    }
  }
};

let languageRequest = 0;
export const changeAppLanguage = async (language) => {
  const lng = language?.startsWith('en') ? 'en' : 'zh-CN';
  const request = ++languageRequest;
  await ensureLoaded(lng);
  if (request === languageRequest) await i18n.changeLanguage(lng);
};

const syncBackendLanguage = (lng) => {
  invoke('set_app_language', { language: lng || 'zh-CN' }).catch(() => {
    // Backend may not be available in web-only mode; ignore failures.
  });
};

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (event) => {
    if (event.key !== 'language' || !event.newValue) return;
    if (event.newValue === i18n.language) return;
    changeAppLanguage(event.newValue).catch(() => {});
  });
}

// load initial language (useLang detector may set i18n.language)
const initialLang = localStorage.getItem('language') || i18n.language || 'zh-CN';
changeAppLanguage(initialLang).catch(() => {});

// when language changes, try to lazy-load resources for it
i18n.on('languageChanged', (lng) => {
  ensureLoaded(lng);
  try {
    localStorage.setItem('language', lng);
  } catch (e) {
    // ignore
  }
  syncBackendLanguage(lng);
});

export default i18n;
