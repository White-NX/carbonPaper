import { useEffect, useState } from 'react';

export function useGeneralPreferenceFlags() {
  const [lowResolutionAnalysis, setLowResolutionAnalysis] = useState(() => localStorage.getItem('lowResolutionAnalysis') === 'true');
  const [sendTelemetryDiagnostics, setSendTelemetryDiagnostics] = useState(() => localStorage.getItem('sendTelemetryDiagnostics') === 'true');

  useEffect(() => {
    try {
      localStorage.setItem('lowResolutionAnalysis', lowResolutionAnalysis ? 'true' : 'false');
    } catch {
      // ignore
    }
  }, [lowResolutionAnalysis]);

  useEffect(() => {
    try {
      localStorage.setItem('sendTelemetryDiagnostics', sendTelemetryDiagnostics ? 'true' : 'false');
    } catch {
      // ignore
    }
  }, [sendTelemetryDiagnostics]);

  return {
    lowResolutionAnalysis,
    toggleLowResolutionAnalysis: () => setLowResolutionAnalysis((value) => !value),
    sendTelemetryDiagnostics,
    toggleTelemetryDiagnostics: () => setSendTelemetryDiagnostics((value) => !value),
  };
}
