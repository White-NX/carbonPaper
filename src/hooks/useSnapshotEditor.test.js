import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useSnapshotEditor } from './useSnapshotEditor';
import { saveEditedScreenshot } from '../lib/monitor_api';

vi.mock('../lib/monitor_api', () => ({ saveEditedScreenshot: vi.fn() }));
vi.mock('../lib/snapshot_editor', async (importOriginal) => ({ ...(await importOriginal()), renderScene: vi.fn() }));

const image = { naturalWidth: 800, naturalHeight: 600 };
const node = { id: 'text-1', kind: 'text', cx: 150, cy: 100, w: 200, h: 40, angle: 0,
  fontSize: 24, color: '#e04e55', text: 'keep this draft' };
const create = (overrides = {}) => renderHook(() => useSnapshotEditor({
  image, screenshotId: 12, imageHash: 'original-hash', enabled: true, ...overrides,
}));
const addDraft = (result) => act(() => {
  result.current.toggleEditing();
  result.current.publish([structuredClone(node)]);
  result.current.record([]);
});

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ measureText: (text) => ({ width: text.length * 10 }) });
  vi.spyOn(HTMLCanvasElement.prototype, 'toDataURL').mockReturnValue('data:image/png;base64,cG5n');
});

describe('snapshot editor save and navigation', () => {
  it('retains the draft and pending navigation when saving fails, and uses the new revision after retry', async () => {
    const navigate = vi.fn();
    const { result } = create();
    addDraft(result);
    act(() => result.current.requestLeave(navigate));
    saveEditedScreenshot.mockRejectedValueOnce(new Error('recognition unavailable'));
    await act(async () => { expect(await result.current.save(true)).toBe(false); });
    expect(result.current.scene).toEqual([node]);
    expect(result.current.dirty).toBe(true);
    expect(result.current.confirmLeave).toBe(true);
    expect(navigate).not.toHaveBeenCalled();

    act(() => result.current.continueEditing());
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 12, image_hash: 'saved-hash' });
    await act(async () => { expect(await result.current.save()).toBe(true); });
    expect(result.current.dirty).toBe(false);
    act(() => result.current.setOption('color', '#458ee8'));
    act(() => {
      result.current.select(node.id);
      result.current.setOption('color', '#458ee8');
      result.current.requestLeave(navigate);
    });
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 12, image_hash: 'next-hash' });
    await act(async () => result.current.save(true));
    expect(saveEditedScreenshot).toHaveBeenLastCalledWith(12, 'saved-hash', 'cG5n');
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it('saves inline text as one undo step and restores the saved state before discarding navigation', async () => {
    const navigate = vi.fn();
    const { result } = create();
    act(() => {
      result.current.toggleEditing();
      result.current.publish([structuredClone(node)]);
      result.current.beginText(result.current.stateRef.current.scene[0], []);
    });
    act(() => result.current.updateText('edited text'));
    act(() => result.current.commitText());
    expect(result.current.undo).toHaveLength(1);
    act(() => result.current.undoAction());
    expect(result.current.scene).toEqual([]);
    act(() => result.current.redoAction());
    expect(result.current.scene[0].text).toBe('edited text');
    saveEditedScreenshot.mockResolvedValueOnce({ screenshot_id: 12, image_hash: 'saved-hash' });
    await act(async () => result.current.save());
    const saved = structuredClone(result.current.scene);
    act(() => {
      result.current.select(node.id);
      result.current.removeSelected();
      result.current.requestLeave(navigate);
    });
    expect(navigate).not.toHaveBeenCalled();
    act(() => result.current.continueEditing());
    expect(result.current.editing).toBe(true);
    act(() => { result.current.requestLeave(navigate); result.current.discard(); });
    expect(result.current.scene).toEqual(saved);
    expect(result.current.dirty).toBe(false);
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it('keeps a durable save successful if the view refresh fails and blocks duplicate saves', async () => {
    let finish;
    saveEditedScreenshot.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const { result } = create({ onSaved: () => { throw new Error('refresh failed'); } });
    const warning = vi.spyOn(console, 'warn').mockImplementation(() => {});
    addDraft(result);
    let saving;
    act(() => { saving = result.current.save(); });
    expect(result.current.busy).toBe(true);
    await act(async () => { expect(await result.current.save()).toBe(false); });
    expect(saveEditedScreenshot).toHaveBeenCalledTimes(1);
    await act(async () => { finish({ screenshot_id: 12, image_hash: 'saved-hash' }); await saving; });
    expect(result.current.dirty).toBe(false);
    expect(result.current.error).toBeNull();
    expect(result.current.busy).toBe(false);
    warning.mockRestore();
  });
});
