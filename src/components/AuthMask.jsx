import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Shield, ShieldCheck, KeyRound } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { formatError } from '../lib/errors';
import { OverlayShell } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

/**
 * Windows Hello 认证遮罩组件
 * 当用户未认证或会话失效时显示
 */
export default function AuthMask({
  isVisible,
  onAuthSuccess,
  authError,
  setAuthError
}) {
  const { t } = useTranslation();
  const [isAuthenticating, setIsAuthenticating] = useState(false);

  const handleUnlock = async () => {
    setIsAuthenticating(true);
    setAuthError(null);

    try {
      // 首先确保凭据已初始化
      await invoke('credential_initialize');

      // 请求 Windows Hello 验证
      const result = await invoke('credential_verify_user');

      if (result) {
        onAuthSuccess?.();
      } else {
        setAuthError(t('authMask.errors.verify_failed'));
      }
    } catch (err) {
      console.error('Authentication error:', err);
      const message = formatError(err);

      if (message.includes('UserCancelled') || message.includes('User cancelled')) {
        setAuthError(t('authMask.errors.cancelled'));
      } else if (message.includes('WindowsHelloNotAvailable')) {
        setAuthError(t('authMask.errors.not_available'));
      } else if (message.includes('KeyNotFound')) {
        // 首次使用，需要创建凭据
        setAuthError(t('authMask.errors.initializing'));
        try {
          await invoke('credential_initialize');
          const retryResult = await invoke('credential_verify_user');
          if (retryResult) {
            onAuthSuccess?.();
            return;
          }
        } catch (retryErr) {
          setAuthError(t('authMask.errors.init_failed', { error: formatError(retryErr) }));
        }
      } else {
        setAuthError(t('authMask.errors.generic_failed', { error: message }));
      }
    } finally {
      setIsAuthenticating(false);
    }
  };

  return (
    <OverlayShell
      open={Boolean(isVisible)}
      layer="gate"
      size="sm"
      icon={Shield}
      title={t('authMask.title')}
      centeredHeader
      bodyClassName="text-center"
    >
      <p className="text-sm leading-relaxed text-ide-muted">{t('authMask.description')}</p>

      <Button size="md" variant="primary" icon={KeyRound} loading={isAuthenticating} onClick={handleUnlock} className="mt-2 w-full">
        {isAuthenticating ? t('authMask.authenticating') : t('authMask.unlock_button')}
      </Button>

      {authError && <Banner tone="error" className="text-left">{authError}</Banner>}

      <div className="flex items-center justify-center gap-2 pt-1 text-xs text-ide-muted/70">
        <ShieldCheck className="h-4 w-4" aria-hidden="true" />
        <span>{t('authMask.encrypted_label')}</span>
      </div>
    </OverlayShell>
  );
}
