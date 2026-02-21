import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Shield, ShieldCheck, Loader2, KeyRound } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

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
      const message = err?.message || String(err);
      
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
          setAuthError(t('authMask.errors.init_failed', { error: retryErr?.message || String(retryErr) }));
        }
      } else {
        setAuthError(t('authMask.errors.generic_failed', { error: message }));
      }
    } finally {
      setIsAuthenticating(false);
    }
  };

  if (!isVisible) return null;

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-md bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl text-center">
        <div className="mx-auto w-14 h-14 rounded-xl bg-ide-bg border border-ide-border flex items-center justify-center mb-4">
          <Shield className="w-7 h-7 text-ide-accent" />
        </div>

        <h2 className="text-lg font-semibold text-ide-text">{t('authMask.title')}</h2>
        <p className="text-sm text-ide-muted mt-2 leading-relaxed">{t('authMask.description')}</p>

        <button
          onClick={handleUnlock}
          disabled={isAuthenticating}
          className="mt-5 w-full flex items-center justify-center gap-2 px-4 py-2 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {isAuthenticating ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              <span>{t('authMask.authenticating')}</span>
            </>
          ) : (
            <>
              <KeyRound className="w-4 h-4" />
              <span>{t('authMask.unlock_button')}</span>
            </>
          )}
        </button>

        {authError && (
          <div className="mt-4 flex items-center justify-center gap-2 px-3 py-2 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-400">
            <span>{authError}</span>
          </div>
        )}

        <div className="mt-4 flex items-center justify-center gap-2 text-xs text-ide-muted/70">
          <ShieldCheck className="w-4 h-4" />
          <span>{t('authMask.encrypted_label')}</span>
        </div>
      </div>
    </div>
  );
}
