import React, { useState } from 'react';
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
        setAuthError('验证失败，请重试');
      }
    } catch (err) {
      console.error('Authentication error:', err);
      const message = err?.message || String(err);
      
      if (message.includes('UserCancelled') || message.includes('User cancelled')) {
        setAuthError('您取消了验证');
      } else if (message.includes('WindowsHelloNotAvailable')) {
        setAuthError('Windows Hello 不可用，请在系统设置中启用');
      } else if (message.includes('KeyNotFound')) {
        // 首次使用，需要创建凭据
        setAuthError('正在初始化安全凭据...');
        try {
          await invoke('credential_initialize');
          const retryResult = await invoke('credential_verify_user');
          if (retryResult) {
            onAuthSuccess?.();
            return;
          }
        } catch (retryErr) {
          setAuthError('初始化失败：' + (retryErr?.message || String(retryErr)));
        }
      } else {
        setAuthError('验证失败：' + message);
      }
    } finally {
      setIsAuthenticating(false);
    }
  };

  if (!isVisible) return null;

  return (
    <div className="absolute top-12 inset-x-0 bottom-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-md bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl text-center">
        <div className="mx-auto w-14 h-14 rounded-xl bg-ide-bg border border-ide-border flex items-center justify-center mb-4">
          <Shield className="w-7 h-7 text-ide-accent" />
        </div>

        <h2 className="text-lg font-semibold text-ide-text">需要 Windows Hello 验证</h2>
        <p className="text-sm text-ide-muted mt-2 leading-relaxed">
          Carbon Paper 使用 Windows Hello 加密您的屏幕截图和 OCR 数据，
          请通过 PIN、指纹或面部识别来解锁。
        </p>

        <button
          onClick={handleUnlock}
          disabled={isAuthenticating}
          className="mt-5 w-full flex items-center justify-center gap-2 px-4 py-2 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {isAuthenticating ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              <span>正在验证...</span>
            </>
          ) : (
            <>
              <KeyRound className="w-4 h-4" />
              <span>使用 Windows Hello 解锁</span>
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
          <span>数据已被加密</span>
        </div>
      </div>
    </div>
  );
}
