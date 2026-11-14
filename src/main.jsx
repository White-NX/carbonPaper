import React, { lazy, Suspense } from 'react'
import ReactDOM from 'react-dom/client'
import './index.css'
import './i18n'

const App = lazy(() => import('./App.jsx'))
const SnapshotPreviewStandalone = lazy(() => import('./components/SnapshotPreviewStandalone.jsx'))
const SettingsWindow = lazy(() => import('./components/settings/SettingsWindow.jsx'))

const params = new URLSearchParams(window.location.search)
const RootComponent = params.get('window') === 'snapshot-preview'
  ? SnapshotPreviewStandalone
  : params.get('window') === 'settings' ? SettingsWindow : App

ReactDOM.createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <Suspense fallback={null}>
      <RootComponent />
    </Suspense>
  </React.StrictMode>,
)
