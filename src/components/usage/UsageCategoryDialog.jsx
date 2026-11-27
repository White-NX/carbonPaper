import React, { useEffect, useId, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CATEGORY_LIST } from '../../lib/categories';
import { usageErrorKey } from '../../lib/usage_api';
import { Dialog } from '../Dialog';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';

export default function UsageCategoryDialog({ app, onClose, onSave }) {
  const { t } = useTranslation();
  const selectId = useId();
  const [category, setCategory] = useState(app.category_is_manual ? app.category : 'auto');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const live = useRef(true);
  useEffect(() => { live.current = true; return () => { live.current = false; }; }, []);
  const save = async () => {
    setSaving(true);
    setError('');
    try {
      await onSave(app.id, category === 'auto' ? null : category, app.generation);
      if (live.current) onClose();
    } catch (failure) {
      if (live.current) setError(String(failure));
    } finally {
      if (live.current) setSaving(false);
    }
  };
  const errorKey = usageErrorKey(error);
  return <Dialog isOpen onClose={onClose} title={t('usage.appCategory')} maxWidth="max-w-sm" disableClose={saving}>
    <div className="space-y-4 p-4">
      <p className="text-sm font-medium text-ide-text">{app.name}</p>
      <p className="text-xs leading-relaxed text-ide-muted">{t('usage.categoryScope')}</p>
      <div className="space-y-2">
        <label htmlFor={selectId} className="block text-xs text-ide-muted">{t('usage.categoryTitle')}</label>
        <select id={selectId} value={category} disabled={saving} onChange={(event) => setCategory(event.target.value)}
          className="w-full rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-sm text-ide-text focus:outline-none focus:ring-1 focus:ring-ide-accent">
          <option value="auto">{t('usage.automaticCategory')}</option>
          {CATEGORY_LIST.map((value) => <option key={value} value={value}>{t(`usage.categories.${value}`)}</option>)}
        </select>
        {category === 'auto' && <p className="text-xs leading-relaxed text-ide-muted">{t('usage.automaticCategoryHint')}</p>}
      </div>
      {error && <Banner tone="error">{t(errorKey === 'usage.errors.unknown' || errorKey === 'usage.errors.USAGE_QUERY_FAILED' ? 'usage.categorySaveFailed' : errorKey)}</Banner>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" disabled={saving} onClick={onClose}>{t('common.cancel')}</Button>
        <Button variant="primary" loading={saving} onClick={save}>{t('common.save')}</Button>
      </div>
    </div>
  </Dialog>;
}
