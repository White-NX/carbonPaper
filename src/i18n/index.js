import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import LanguageDetector from 'i18next-browser-languagedetector';

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

// load initial language (useLang detector may set i18n.language)
const initialLang = localStorage.getItem('language') || i18n.language || 'zh-CN';
ensureLoaded(initialLang).then(() => {
  i18n.changeLanguage(initialLang).catch(() => {});
});

// when language changes, try to lazy-load resources for it
i18n.on('languageChanged', (lng) => {
  ensureLoaded(lng);
  try {
    localStorage.setItem('language', lng);
  } catch (e) {
    // ignore
  }
});

export default i18n;
