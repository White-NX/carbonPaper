import React from 'react';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import SnapshotViewer from './SnapshotViewer';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('./DetailCard', () => ({ default: () => <div>Snapshot metadata</div> }));
vi.mock('./InspectorImage', () => ({ InspectorImage: ({ item }) => <img src={item.imageUrl} alt="Snapshot" /> }));
vi.mock('./SnapshotSourceAction', () => ({ default: () => null }));

const item = { screenshot_id: 42, captured_at_ms: 1000, process_name: 'Editor' };
const details = { record: { created_at: '2026-01-01T10:00:00Z' }, ocr_results: [{ text: 'First line' }, { text: 'Second line' }] };
beforeEach(() => localStorage.clear());

it('copies recognized text and navigates using the stored UTC capture time', async () => {
  const copy = vi.fn(async () => {}), timeline = vi.fn(), standalone = vi.fn();
  render(<SnapshotViewer item={item} details={details} imageSrc="snapshot.png" onCopyText={copy} onOpenTimeline={timeline} onOpenStandalone={standalone} />);
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.copyOcr' }));
  await screen.findByRole('button', { name: 'snapshotPreview.copied' });
  expect(copy).toHaveBeenCalledWith('First line\nSecond line');
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.openTimeline' }));
  expect(timeline).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, captured_at_ms: Date.parse(details.record.created_at) }));
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.openStandalone' }));
  expect(standalone).toHaveBeenCalledWith(expect.objectContaining({ captured_at_ms: Date.parse(details.record.created_at) }));
});

it('navigates a path-only visual preview using the screenshot id resolved from storage', () => {
  const timeline = vi.fn(), standalone = vi.fn();
  render(<SnapshotViewer item={{ id: 'd766e83ee7354fb0d83f3dbf6d68c504', image_path: 'memory://visual' }}
    details={{ ...details, record: { ...details.record, id: 42 } }} onOpenTimeline={timeline} onOpenStandalone={standalone} />);
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.openTimeline' }));
  expect(timeline).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, id: 42, captured_at_ms: Date.parse(details.record.created_at) }));
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.openStandalone' }));
  expect(standalone).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, id: 42 }));
});

it('uses keyboard tabs and remembers a manually hidden information panel', () => {
  const { rerender } = render(<SnapshotViewer item={item} details={details} />);
  const toggle = screen.getByRole('button', { name: 'snapshotPreview.details' });
  expect(toggle).toHaveAttribute('aria-expanded', 'false');
  fireEvent.click(toggle);
  const tabs = screen.getByRole('tablist');
  fireEvent.keyDown(tabs, { key: 'ArrowRight' });
  expect(screen.getByRole('tab', { name: 'snapshotPreview.ocrText' })).toHaveFocus();
  expect(screen.getByRole('textbox', { name: 'snapshotPreview.ocrText' })).toHaveValue('First line\nSecond line');
  fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.hideDetails' }));
  expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
  expect(toggle).toHaveFocus();
  rerender(<SnapshotViewer item={{ ...item, screenshot_id: 43 }} details={details} />);
  expect(screen.getByRole('button', { name: 'snapshotPreview.details' })).toHaveAttribute('aria-expanded', 'false');
});

it('keeps deletion behind an explicit confirmation and exposes failures', async () => {
  const remove = vi.fn(async () => { throw new Error('delete failed'); });
  render(<SnapshotViewer item={item} details={details} onDeleteRecord={remove} />);
  fireEvent.click(screen.getByRole('button', { name: 'previewAction.moreDeleteOptions' }));
  fireEvent.click(screen.getByRole('menuitem', { name: 'previewAction.deleteThisRecord' }));
  expect(remove).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'previewAction.delete' }));
  await waitFor(() => expect(remove).toHaveBeenCalledWith(42));
  expect(await screen.findByRole('alert')).toHaveTextContent('delete failed');
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});
