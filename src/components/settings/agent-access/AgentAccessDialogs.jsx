import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle } from 'lucide-react';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Dialog } from '../../Dialog';

export default function AgentAccessDialogs({
  showPrivacyDialog,
  onClosePrivacyDialog,
  confirmText,
  onConfirmTextChange,
  confirmTextExpected,
  onConfirmEnable,
  showResetConfirm,
  onCloseResetConfirm,
  onResetToken,
}) {
  const { t } = useTranslation();

  return (
    <>
      <Dialog
        isOpen={showPrivacyDialog}
        onClose={onClosePrivacyDialog}
        title={t('settings.ai_embedding.privacy_warning.title')}
        maxWidth="max-w-md"
      >
        <div className="p-4 space-y-4">
          <div className="p-3 bg-ide-warning-bg border border-ide-warning-border rounded-lg flex items-start gap-2">
            <AlertTriangle className="w-5 h-5 text-ide-warning shrink-0 mt-0.5" />
            <p className="text-xs text-ide-text leading-relaxed whitespace-pre-line">
              {t('settings.ai_embedding.privacy_warning.message')}
            </p>
          </div>

          <div className="space-y-2">
            <p className="text-xs text-ide-muted">
              {t('settings.ai_embedding.privacy_warning.confirm_prompt')}
            </p>
            <p className="text-xs text-ide-text font-medium px-2 py-1.5 bg-ide-panel border border-ide-border rounded select-all">
              {confirmTextExpected}
            </p>
            <input
              type="text"
              value={confirmText}
              onChange={(e) => onConfirmTextChange(e.target.value)}
              className="w-full px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text focus:outline-none focus:border-ide-accent"
              placeholder=""
              autoFocus
            />
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <button
              onClick={onClosePrivacyDialog}
              className="px-4 py-2 text-sm text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded-lg transition-colors"
            >
              {t('settings.ai_embedding.privacy_warning.cancel_button')}
            </button>
            <button
              onClick={onConfirmEnable}
              disabled={confirmText !== confirmTextExpected}
              className="px-4 py-2 text-sm bg-ide-accent text-white rounded-lg hover:bg-ide-accent/80 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
            >
              {t('settings.ai_embedding.privacy_warning.confirm_button')}
            </button>
          </div>
        </div>
      </Dialog>

      <ConfirmDialog
        isOpen={showResetConfirm}
        onCancel={onCloseResetConfirm}
        onConfirm={onResetToken}
        title={t('settings.ai_embedding.token.reset_confirm_title')}
        message={t('settings.ai_embedding.token.reset_confirm_message')}
        confirmVariant="danger"
      />
    </>
  );
}
