import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App.jsx'
import SnapshotPreviewStandalone from './components/SnapshotPreviewStandalone.jsx'
import './index.css'
import './i18n'

const params = new URLSearchParams(window.location.search)
const RootComponent = params.get('window') === 'snapshot-preview'
  ? SnapshotPreviewStandalone
  : App

ReactDOM.createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <RootComponent />
  </React.StrictMode>,
)
