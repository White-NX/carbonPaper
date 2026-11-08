import { useEffect, useState } from 'react';
import { setAppTheme } from '../lib/settings_api';

export function useAppTheme() {
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return localStorage.getItem('theme') === 'dark' ||
        (!localStorage.getItem('theme') && window.matchMedia('(prefers-color-scheme: dark)').matches);
    }
    return true;
  });

  useEffect(() => {
    const syncTheme = (event) => {
      if (event.key === 'theme') setDarkMode(event.newValue === 'dark');
    };
    window.addEventListener('storage', syncTheme);
    return () => window.removeEventListener('storage', syncTheme);
  }, []);

  useEffect(() => {
    if (darkMode) {
      document.documentElement.classList.add('dark');
      localStorage.setItem('theme', 'dark');
    } else {
      document.documentElement.classList.remove('dark');
      localStorage.setItem('theme', 'light');
    }
    void setAppTheme(darkMode ? 'dark' : 'light').catch((error) => {
      console.warn('Failed to synchronize the native theme:', error);
    });
  }, [darkMode]);

  return { darkMode, setDarkMode };
}
