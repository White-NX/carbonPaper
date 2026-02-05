import React, { useState, useEffect } from 'react';
import { Shield, ShieldCheck, Clock, Info, ChevronDown, AlertTriangle } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

// PIN 暂存时间选项（单位：秒）
const SESSION_TIMEOUT_OPTIONS = [
  { value: 300, label: '5 分钟', description: '较高安全性' },
  { value: 900, label: '15 分钟', description: '推荐' },
  { value: 3600, label: '1 小时', description: '方便使用' },
  { value: 86400, label: '1 天', description: '较低安全性' },
  { value: -1, label: '直到关闭', description: '不推荐', warning: true },
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
  const [showTimeoutDropdown, setShowTimeoutDropdown] = useState(false);
  const [isLocking, setIsLocking] = useState(false);

  // 获取当前选中的超时选项
  const currentOption = SESSION_TIMEOUT_OPTIONS.find(opt => opt.value === sessionTimeout) 
    || SESSION_TIMEOUT_OPTIONS[1]; // 默认 15 分钟

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
      {/* Windows Hello 保护状态提示 */}
      <div className="p-4 bg-gradient-to-r from-blue-500/10 to-indigo-500/10 border border-blue-500/20 rounded-xl">
        <div className="flex items-start gap-4">
          <div className="w-10 h-10 rounded-lg bg-blue-500/20 flex items-center justify-center shrink-0">
            <ShieldCheck className="w-5 h-5 text-blue-400" />
          </div>
          <div className="flex-1 min-w-0">
            <h3 className="text-sm font-semibold text-ide-text flex items-center gap-2">
              Windows Hello 保护已启用
              <span className="px-2 py-0.5 bg-green-500/20 text-green-400 text-xs rounded-full font-normal">
                活跃
              </span>
            </h3>
            <p className="text-xs text-ide-muted mt-1 leading-relaxed">
              您的屏幕截图和 OCR 数据已使用 Windows Hello 凭据加密。只有通过您的 PIN、指纹或面部识别验证后才能访问数据。
            </p>
          </div>
        </div>
      </div>

      {/* PIN 暂存时间设置 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Clock className="w-4 h-4" />
          解锁有效期
        </label>
        
        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">自动锁定时间</p>
              <p className="text-xs text-ide-muted mt-1">
                设置在用户无操作多长时间后自动锁定并需要重新验证
              </p>
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
                当前会话：{isSessionValid ? '已解锁' : '已锁定'}
              </span>
            </div>
            
            {isSessionValid && (
              <button
                onClick={handleLockNow}
                disabled={isLocking}
                className="px-3 py-1.5 text-xs text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded-lg transition-colors disabled:opacity-50"
              >
                {isLocking ? '锁定中...' : '立即锁定'}
              </button>
            )}
          </div>
        </div>
      </div>

      {/* 安全说明 */}
      <div className="p-4 bg-ide-panel/50 border border-ide-border/50 rounded-xl">
        <div className="flex items-start gap-3">
          <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
          <div className="text-xs text-ide-muted space-y-2">
            <p>
              <strong className="text-ide-text">关于数据安全：</strong> Carbon Paper 使用 Windows Hello 
              密钥凭据管理器来加密您的截图和 OCR 文本数据。
            </p>
            <p>
              • 所有截图文件使用 AES-256-GCM 加密存储<br />
              • 数据库使用 SQLCipher 整库加密<br />
              • 加密密钥由 Windows Hello 保护，永不离开您的设备
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
