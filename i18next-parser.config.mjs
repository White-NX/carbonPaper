export default {
  locales: ['zh-CN', 'en'],
  defaultLocale: 'zh-CN',
  output: 'src/i18n/locales/$LOCALE.json',
  input: ['src/**/*.{js,jsx}'],
  sort: true,
  createOldCatalogs: false,
  keySeparator: '.',
  namespaceSeparator: false,
  // keep existing translations, only add missing keys
  keepRemoved: false,
}
