module.exports = {
  input: [
    'src/**/*.{js,jsx,ts,tsx}'
  ],
  output: './',
  options: {
    debug: false,
    func: {
      list: ['i18n.t', 'i18n.translate', 't'],
      extensions: ['.js', '.jsx', '.ts', '.tsx']
    },
    lngs: ['en', 'zh-CN'],
    ns: ['translation'],
    defaultLng: 'en',
    defaultNs: 'translation',
    resource: {
      loadPath: 'src/i18n/locales/{{lng}}.json',
      savePath: 'src/i18n/locales/{{lng}}.json',
      jsonIndent: 2
    }
  }
};
