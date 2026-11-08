import React, { useState } from 'react';
import { useOverlaySlot } from '../../overlay/coordinator';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';
import { useSecurityLevel } from './useSecurityLevel';
import { useSecurityWizard } from './useSecurityWizard';
import SecurityWizard from './SecurityWizard';

export default function SecurityOnboardingOverlay({ enabled, onSessionTimeoutChange }) {
  const [closed, setClosed] = useState(false);
  const security = useSecurityLevel({ enabled: enabled && !closed, onSessionTimeoutChange });
  const wants = Boolean(enabled && !closed && security.policy && !security.policy.intro_seen);
  const shown = useOverlaySlot('securityOnboarding', wants);
  const wizard = useSecurityWizard({ open: wants, security, onClose: () => setClosed(true) });
  useTauriEventListener('security-intro-seen', () => setClosed(true), [], enabled);
  return <SecurityWizard open={shown} wizard={wizard} presentation="startup" />;
}
