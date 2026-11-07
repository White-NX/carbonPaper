// This classic script runs before the React module graph is loaded. Keep it
// dependency-free; the native API is Tauri's documented withGlobalTauri API.
(() => {
  const documentRoot = document.documentElement;
  const appRoot = document.getElementById('root');
  const screen = document.getElementById('startup-screen');
  const readPreference = (key) => {
    try { return localStorage.getItem(key); } catch { return null; }
  };
  const savedTheme = readPreference('theme');
  const dark = savedTheme === 'dark' || (!savedTheme && window.matchMedia('(prefers-color-scheme: dark)').matches);
  documentRoot.classList.toggle('dark', dark);

  const params = new URLSearchParams(window.location.search);
  if (params.get('window') && params.get('window') !== 'main') {
    screen?.remove();
    return;
  }
  if (!screen || !appRoot) return;

  let phase = 'loading';
  let windowReadyRequested = false;
  const generation = Number(params.get('startup') || 0);
  const language = (readPreference('language') || navigator.language || 'zh-CN').toLowerCase().startsWith('en') ? 'en' : 'zh-CN';
  const native = window.__TAURI__;
  const controls = screen.querySelectorAll('[data-startup-action]');
  appRoot.setAttribute('inert', '');
  appRoot.setAttribute('aria-hidden', 'true');
  documentRoot.dataset.startup = phase;

  function setLabels(labels) {
    if (!labels || !screen.isConnected) return;
    const tagline = screen.querySelector('[data-startup-tagline]');
    if (labels.tagline) tagline.textContent = labels.tagline;
    for (const button of controls) {
      const label = labels[button.dataset.startupAction];
      if (label) {
        button.setAttribute('aria-label', label);
        button.title = label;
      }
    }
  }

  // WebView2 may throttle animation frames while its host window is hidden.
  // A bounded fallback lets that window appear without waiting indefinitely.
  function afterPaint(callback) {
    let firstFrame;
    let secondFrame;
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      clearTimeout(fallback);
      cancelAnimationFrame(firstFrame);
      cancelAnimationFrame(secondFrame);
      callback();
    };
    const fallback = setTimeout(finish, document.hidden ? 0 : 100);
    firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(finish);
    });
  }

  function notifyWindowReady() {
    if (windowReadyRequested || !native?.core) return;
    windowReadyRequested = true;
    native.core.invoke('main_window_ready', { generation, language })
      .then(setLabels)
      .catch((error) => console.warn('Failed to present the startup window', error));
  }

  function finishHandoff() {
    if (phase !== 'leaving') return;
    phase = 'complete';
    screen.remove();
    delete documentRoot.dataset.startup;
  }

  function handoff() {
    if (phase !== 'loading') return;
    phase = 'handoff';
    documentRoot.dataset.startup = phase;
    appRoot.removeAttribute('inert');
    appRoot.removeAttribute('aria-hidden');
    screen.setAttribute('inert', '');
    screen.setAttribute('aria-hidden', 'true');
    afterPaint(() => {
      phase = 'leaving';
      documentRoot.dataset.startup = phase;
      notifyWindowReady();
      if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
        finishHandoff();
      } else {
        screen.addEventListener('transitionend', (event) => {
          if (event.target === screen && event.propertyName === 'opacity') finishHandoff();
        });
        setTimeout(finishHandoff, 180);
      }
    });
  }

  window.__CARBONPAPER_STARTUP__ = Object.freeze({ setLabels, handoff });

  for (const button of controls) {
    button.disabled = !native?.core;
    button.addEventListener('click', () => {
      const action = button.dataset.startupAction === 'minimize'
        ? native.window.getCurrentWindow().minimize()
        : native.core.invoke('hide_to_tray');
      action.catch((error) => console.warn('Failed to apply the startup window action', error));
    });
  }

  const mark = screen.querySelector('[data-startup-asset]');
  const markReady = typeof mark?.decode === 'function' ? mark.decode().catch(() => {}) : Promise.resolve();
  markReady.then(() => afterPaint(notifyWindowReady));
})();
