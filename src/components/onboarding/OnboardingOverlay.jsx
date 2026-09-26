import React from 'react';
import { useOverlaySlot } from '../overlay';
import { MODEL_DOWNLOAD_MB } from '../../lib/modelSizes';
import OnboardingWizard from './OnboardingWizard';
import WhatsNewDialog from './WhatsNewDialog';
import { RERANKER_MODEL_ID } from './onboardingPlan';

/**
 * Puts the wizard or the what's-new page on screen when the coordinator
 * allows. State lives in `useOnboarding`, which App owns because the monitor
 * lifecycle reads its `captureHeld`.
 */
export default function OnboardingOverlay({ onboarding, required }) {
  const shown = useOverlaySlot('onboarding', onboarding.open);
  if (!shown) return null;

  if (onboarding.mode === 'whats_new') return <WhatsNewDialog onboarding={onboarding} />;

  // What "use recommended settings" will download, shown on the welcome page.
  const rerankerMb = onboarding.context.rerankerInstalled ? 0 : MODEL_DOWNLOAD_MB[RERANKER_MODEL_ID];
  const downloadMb = (required.needDownload ? required.sizeMb : 0) + rerankerMb;

  return <OnboardingWizard onboarding={onboarding} required={required} downloadMb={downloadMb} />;
}
