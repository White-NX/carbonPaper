/** @type {import('tailwindcss').Config} */
export default {
  content: [
    './index.html',
    './src/**/*.{js,ts,jsx,tsx}',
  ],
  theme: {
    extend: {
      colors: {
        'ide-bg': 'var(--ide-bg)',
        'ide-panel': 'var(--ide-panel)',
        'ide-border': 'var(--ide-border)',
        'ide-text': 'var(--ide-text)',
        'ide-muted': 'var(--ide-muted)',
        'ide-accent': 'var(--ide-accent)',
        'ide-active': 'var(--ide-active)',
        'ide-hover': 'var(--ide-hover)',
        'ide-error': 'var(--ide-error)',
        'ide-success': 'var(--ide-success)',
      },
      boxShadow: {
        card: '0 10px 30px rgba(0,0,0,0.35)',
      },
      fontFamily: {
        sans: ['Inter', 'Segoe UI', 'system-ui', 'sans-serif'],
        mono: ['JetBrains Mono', 'SFMono-Regular', 'Menlo', 'monospace'],
      },
    },
  },
  plugins: [],
};
