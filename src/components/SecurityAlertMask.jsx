import React, { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldAlert, AlertTriangle, X } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

/**
 * 监视器完整性告警全屏蒙层。
 *
 * 触发渠道：Rust 端 emit('security-alert', { code, message, detail })。
 * 严重场景：当 monitor.pyz 被篡改、缺失等真实安全事件时，必须强制用户感知，
 * 因此用全屏 backdrop-blur 蒙层 + 中央卡片 + 红色告警色调（与 AuthMask 同语言但更醒目）。
 *
 * 区分：
 *   - 真实告警（MONITOR_PYZ_TAMPERED / MONITOR_PYZ_MISSING）：只允许「退出应用」
 *   - 调试触发（DEBUG_MANUAL_TRIGGER）：允许「关闭」
 */
export default function SecurityAlertMask({ alert, onDismiss }) {
  const { t } = useTranslation();
  const isDebug = alert?.code === 'DEBUG_MANUAL_TRIGGER';

  // 真实告警时禁止 Esc/键盘关闭、阻断点击穿透
  useEffect(() => {
    if (!alert || isDebug) return;
    const blockEsc = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener('keydown', blockEsc, true);
    return () => window.removeEventListener('keydown', blockEsc, true);
  }, [alert, isDebug]);

  if (!alert) return null;

  const handleExit = async () => {
    try {
      await invoke('close_process');
    } catch (err) {
      console.error('Failed to exit app:', err);
      // 兜底：直接关窗口
      try {
        const { getCurrentWindow } = await import('@tauri-apps/api/window');
        await getCurrentWindow().close();
      } catch (e) {
        console.error('Window close fallback failed:', e);
      }
    }
  };

  return (
    <div
      className="fixed inset-0 z-[90] flex flex-col items-center justify-center bg-black/70 backdrop-blur-md text-ide-text"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="security-alert-title"
    >
      {/* 红色脉冲光晕背景 */}
      <div
        className="absolute inset-0 pointer-events-none"
        style={{
          background:
            'radial-gradient(circle at 50% 50%, rgba(220,38,38,0.18) 0%, rgba(0,0,0,0) 60%)',
          animation: 'security-pulse 2.4s ease-in-out infinite',
        }}
      />
      <style>{`
        @keyframes security-pulse {
          0%, 100% { opacity: 0.55; }
          50%      { opacity: 1; }
        }
      `}</style>

      <div className="relative w-full max-w-lg mx-6 bg-ide-panel border-2 border-red-500/60 rounded-2xl p-7 shadow-2xl shadow-red-500/20">
        <div className="flex items-start gap-4">
          <div className="shrink-0 w-14 h-14 rounded-xl bg-red-500/15 border border-red-500/40 flex items-center justify-center">
            <ShieldAlert className="w-7 h-7 text-red-400" />
          </div>
          <div className="flex-1 min-w-0">
            <h2 id="security-alert-title" className="text-lg font-bold text-red-300">
              {isDebug
                ? t('securityAlert.debug_title', 'Security alert — debug trigger')
                : t('securityAlert.title', 'Monitor integrity compromised')}
            </h2>
            <p className="text-sm text-ide-muted mt-1 leading-relaxed">
              {alert.message || t(
                'securityAlert.default_message',
                'A required monitor component failed its integrity check. Startup has been blocked to protect your data.'
              )}
            </p>
          </div>
        </div>

        {/* 详情面板 */}
        <div className="mt-5 bg-ide-bg border border-ide-border/60 rounded-lg p-3 space-y-2">
          {alert.code && (
            <div className="flex items-center gap-2 text-xs">
              <span className="text-ide-muted">{t('securityAlert.code_label', 'Code')}:</span>
              <code className="font-mono text-red-400 select-all">{alert.code}</code>
            </div>
          )}
          {alert.detail && (
            <div className="text-xs">
              <div className="text-ide-muted mb-1">{t('securityAlert.detail_label', 'Detail')}:</div>
              <pre className="font-mono text-ide-text/80 whitespace-pre-wrap break-all max-h-32 overflow-y-auto pr-1">
                {alert.detail}
              </pre>
            </div>
          )}
        </div>

        {/* 用户应做什么的建议（仅真实场景） */}
        {!isDebug && (
          <div className="mt-4 flex items-start gap-2 p-3 bg-red-500/5 border border-red-500/20 rounded-lg">
            <AlertTriangle className="w-4 h-4 text-red-400 shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">
              {t(
                'securityAlert.recovery_hint',
                'If you did not install or update CarbonPaper yourself, please reinstall from a trusted source. A log entry has been written to security.log under your CarbonPaper AppData directory.'
              )}
            </p>
          </div>
        )}

        {/* 操作按钮 */}
        <div className="mt-6 flex items-center justify-end gap-2">
          {isDebug ? (
            <button
              onClick={onDismiss}
              className="flex items-center gap-2 px-4 py-2 bg-ide-bg border border-ide-border hover:bg-ide-hover text-ide-text rounded-lg text-sm font-medium transition-colors"
            >
              <X className="w-4 h-4" />
              {t('securityAlert.dismiss', 'Dismiss')}
            </button>
          ) : (
            <button
              onClick={handleExit}
              className="flex items-center gap-2 px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded-lg text-sm font-medium transition-colors"
            >
              {t('securityAlert.exit', 'Exit application')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
