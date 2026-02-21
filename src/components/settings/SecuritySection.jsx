import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Shield, ShieldCheck, Clock, Info, ChevronDown, AlertTriangle } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

// 会话超时选项的固定值; 标签/描述由 i18n 在组件内生成
const SESSION_TIMEOUT_VALUES = [
  { value: 300, key: '5m' },
  { value: 900, key: '15m' },
  { value: 3600, key: '1h' },
  { value: 86400, key: '1d' },
  { value: -1, key: 'until_close', warning: true },
];

/**
 * 安全设置组件
 * 显示 Windows Hello 保护状态和 PIN 暂存时间设置
 */
export default function SecuritySection({
  sessionTimeout,
  onSessionTimeoutChange,
  isSessionValid,
  onLockSession,
}) {
  const { t } = useTranslation();
  const [showTimeoutDropdown, setShowTimeoutDropdown] = useState(false);
  const [isLocking, setIsLocking] = useState(false);

  // 构建本地化的超时选项
  const SESSION_TIMEOUT_OPTIONS = SESSION_TIMEOUT_VALUES.map((opt) => ({
    value: opt.value,
    label: t(`settings.security.session.options.${opt.key}.label`),
    description: t(`settings.security.session.options.${opt.key}.description`),
    warning: Boolean(opt.warning),
  }));

  // 获取当前选中的超时选项
  const currentOption = SESSION_TIMEOUT_OPTIONS.find((opt) => opt.value === sessionTimeout) || SESSION_TIMEOUT_OPTIONS[1]; // 默认 15 分钟

  const handleTimeoutChange = async (value) => {
    setShowTimeoutDropdown(false);
    onSessionTimeoutChange(value);
    
    // 保存到本地存储
    try {
      localStorage.setItem('sessionTimeout', String(value));
      // 通知后端更新超时时间
      await invoke('credential_set_session_timeout', { timeout: value });
    } catch (err) {
      console.warn('Failed to save session timeout:', err);
    }
  };

  const handleLockNow = async () => {
    setIsLocking(true);
    try {
      await invoke('credential_lock_session');
      onLockSession?.();
    } catch (err) {
      console.error('Failed to lock session:', err);
    } finally {
      setIsLocking(false);
    }
  };

  // 点击外部关闭下拉菜单
  useEffect(() => {
    const handleClickOutside = () => setShowTimeoutDropdown(false);
    if (showTimeoutDropdown) {
      document.addEventListener('click', handleClickOutside);
      return () => document.removeEventListener('click', handleClickOutside);
    }
  }, [showTimeoutDropdown]);

  return (
    <div className="space-y-6">
      <div className="p-4 bg-gradient-to-r border border-ide-border rounded-xl">
        <div className="flex items-start gap-4">
          <div className="w-10 h-10 rounded-lg bg-blue-500/20 flex items-center justify-center shrink-0">
            <ShieldCheck className="w-5 h-5 text-blue-400" />
          </div>
          <div className="flex-1 min-w-0">
              <h3 className="text-sm font-semibold text-ide-text flex items-center gap-2">
              {t('settings.security.protection.title')}
            </h3>
            <p className="text-xs text-ide-muted mt-1 leading-relaxed">
              {t('settings.security.protection.description')}
            </p>
          </div>
        </div>
      </div>

      {/* PIN 暂存时间设置 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          {t('settings.security.unlock_label')}
        </label>
        
        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.security.session.title')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.security.session.description')}</p>
            </div>
            
            {/* 下拉选择器 */}
            <div className="relative">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setShowTimeoutDropdown(!showTimeoutDropdown);
                }}
                className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[140px]"
              >
                <span className="flex-1 text-left">{currentOption.label}</span>
                <ChevronDown className={`w-4 h-4 text-ide-muted transition-transform ${showTimeoutDropdown ? 'rotate-180' : ''}`} />
              </button>
              
              {showTimeoutDropdown && (
                <div 
                  className="absolute right-0 top-full mt-2 w-56 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                >
                  {SESSION_TIMEOUT_OPTIONS.map((option) => (
                    <button
                      key={option.value}
                      onClick={() => handleTimeoutChange(option.value)}
                      className={`w-full px-4 py-3 text-left hover:bg-ide-hover transition-colors flex items-center justify-between gap-2 ${
                        option.value === sessionTimeout ? 'bg-ide-accent/10' : ''
                      }`}
                    >
                      <div>
                        <div className="text-sm text-ide-text flex items-center gap-2">
                          {option.label}
                          {option.warning && (
                            <AlertTriangle className="w-3.5 h-3.5 text-amber-400" />
                          )}
                        </div>
                        <div className={`text-xs ${option.warning ? 'text-amber-400/70' : 'text-ide-muted'}`}>
                          {option.description}
                        </div>
                      </div>
                      {option.value === sessionTimeout && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          {/* 当前会话状态 */}
          <div className="flex items-center justify-between pt-3 border-t border-ide-border/50">
            <div className="flex items-center gap-2">
              <div className={`w-2 h-2 rounded-full ${isSessionValid ? 'bg-green-400' : 'bg-red-400'}`} />
              <span className="text-xs text-ide-muted">
                {t('settings.security.session.current.label')} {isSessionValid ? t('settings.security.session.current.unlocked') : t('settings.security.session.current.locked')}
              </span>
            </div>
            
            {isSessionValid && (
              <button
                onClick={handleLockNow}
                disabled={isLocking}
                className="px-3 py-1.5 text-xs text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded-lg transition-colors disabled:opacity-50"
              >
                {isLocking ? t('settings.security.session.locking') : t('settings.security.session.lock_now')}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
