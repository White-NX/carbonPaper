import { useCallback, useEffect, useReducer, useRef } from 'react';
import { saveEditedScreenshot } from '../lib/monitor_api';
import {
  clamp, cloneScene, constrainObject, EDITOR_HISTORY_LIMIT, fitText, renderScene,
  scenesEqual, TEXT_FONT,
} from '../lib/snapshot_editor';

export function useSnapshotEditor({ image, screenshotId, imageHash, enabled, onSaved, onNavigationGuardChange, placeholder }) {
  const [revision, refresh] = useReducer((value) => value + 1, 0);
  const optionsRef = useRef(null);
  const stateRef = useRef({
    editing: false, tool: 'select', scene: [], saved: [], undo: [], redo: [],
    selectedId: null, editingId: null, textBefore: null, propertyBefore: null,
    color: '#e04e55', brushSize: 4, eraseSize: 28, fontSize: 24, blocks: 14,
    busy: false, error: null, savedNotice: false, confirmLeave: false, imageHash: null,
    baseImage: null, baseSource: null,
  });
  const state = stateRef.current;
  optionsRef.current = { image: state.baseImage || image, sourceImage: image, screenshotId, enabled, onSaved, placeholder };
  if (!state.imageHash && imageHash) state.imageHash = imageHash;
  const pendingActionRef = useRef(null);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  const notify = useCallback(() => { if (mountedRef.current) refresh(); }, []);
  useEffect(() => {
    const current = stateRef.current;
    if (current.baseImage && image && image !== current.baseSource && imageHash === current.imageHash) {
      current.baseImage = null;
      current.baseSource = null;
      notify();
    }
  }, [image, imageHash, notify]);
  const record = useCallback((before) => {
    const current = stateRef.current;
    if (!scenesEqual(before, current.scene)) {
      current.undo.push(before);
      if (current.undo.length > EDITOR_HISTORY_LIMIT) current.undo.shift();
      current.redo = [];
      current.savedNotice = false;
      current.error = null;
    }
    notify();
  }, [notify]);

  const measure = useCallback((text, fontSize) => {
    const context = document.createElement('canvas').getContext('2d');
    context.font = '500 ' + fontSize + 'px ' + TEXT_FONT;
    return context.measureText(text).width;
  }, []);

  const fit = useCallback((node) => {
    const original = optionsRef.current.image;
    if (!original) return;
    const width = original.naturalWidth || original.width;
    const height = original.naturalHeight || original.height;
    if (node.kind === 'text') {
      const left = node.cx - node.w / 2;
      const top = node.cy - node.h / 2;
      fitText(node, width, (text) => measure(text, node.fontSize), optionsRef.current.placeholder);
      node.cx = left + node.w / 2;
      node.cy = top + node.h / 2;
    }
    constrainObject(node, width, height);
  }, [measure]);

  const commitText = useCallback(() => {
    const current = stateRef.current;
    if (!current.editingId) return;
    const node = current.scene.find((item) => item.id === current.editingId);
    const before = current.textBefore || cloneScene(current.scene);
    if (node && !node.text.trim()) {
      current.scene = current.scene.filter((item) => item.id !== node.id);
      current.selectedId = null;
    }
    current.editingId = null;
    current.textBefore = null;
    record(before);
  }, [record]);

  const publish = useCallback((scene) => {
    stateRef.current.scene = scene;
    notify();
  }, [notify]);

  const chooseTool = useCallback((tool) => {
    commitText();
    const current = stateRef.current;
    if (current.busy) return;
    current.tool = tool;
    const selected = current.scene.find((node) => node.id === current.selectedId);
    if (selected?.kind !== tool) current.selectedId = null;
    notify();
  }, [commitText, notify]);

  const beginText = useCallback((node, before = cloneScene(stateRef.current.scene)) => {
    const current = stateRef.current;
    if (current.editingId === node.id) return;
    commitText();
    current.selectedId = node.id;
    current.editingId = node.id;
    current.textBefore = before;
    current.tool = 'text';
    fit(node);
    notify();
  }, [commitText, fit, notify]);

  const updateText = useCallback((text) => {
    const current = stateRef.current;
    const node = current.scene.find((item) => item.id === current.editingId);
    if (!node || current.busy) return;
    node.text = text;
    fit(node);
    notify();
  }, [fit, notify]);

  const select = useCallback((id) => {
    commitText();
    stateRef.current.selectedId = id;
    notify();
  }, [commitText, notify]);

  const removeSelected = useCallback(() => {
    commitText();
    const current = stateRef.current;
    if (current.busy || !current.selectedId) return;
    const before = cloneScene(current.scene);
    current.scene = current.scene.filter((node) => node.id !== current.selectedId);
    current.selectedId = null;
    record(before);
  }, [commitText, record]);

  const beginProperty = useCallback(() => {
    commitText();
    stateRef.current.propertyBefore ||= cloneScene(stateRef.current.scene);
  }, [commitText]);

  const setOption = useCallback((key, value, continuous = false) => {
    const current = stateRef.current;
    if (current.busy) return;
    commitText();
    const before = current.propertyBefore || cloneScene(current.scene);
    const node = current.scene.find((item) => item.id === current.selectedId);
    if (key !== 'angle') current[key] = value;
    if (node && ((node.kind === 'text' && ['color', 'fontSize', 'angle'].includes(key))
      || (node.kind === 'mosaic' && ['blocks', 'angle'].includes(key)))) {
      node[key] = key === 'angle' ? clamp(Number(value), -180, 180) : value;
      fit(node);
    }
    if (continuous) {
      current.propertyBefore = before;
      notify();
    } else {
      current.propertyBefore = null;
      record(before);
    }
  }, [commitText, fit, notify, record]);

  const commitProperty = useCallback(() => {
    const current = stateRef.current;
    if (!current.propertyBefore) return;
    const before = current.propertyBefore;
    current.propertyBefore = null;
    record(before);
  }, [record]);

  const changeHistory = useCallback((redo = false) => {
    const current = stateRef.current;
    if (current.busy) return;
    commitText();
    commitProperty();
    const source = redo ? current.redo : current.undo;
    const target = redo ? current.undo : current.redo;
    if (!source.length) return;
    target.push(cloneScene(current.scene));
    current.scene = source.pop();
    current.selectedId = null;
    current.savedNotice = false;
    current.error = null;
    notify();
  }, [commitText, commitProperty, notify]);

  const exit = useCallback(() => {
    const current = stateRef.current;
    current.editing = false;
    current.selectedId = null;
    current.tool = 'select';
    current.confirmLeave = false;
    const action = pendingActionRef.current;
    pendingActionRef.current = null;
    notify();
    action?.();
  }, [notify]);

  const requestLeave = useCallback((action) => {
    const current = stateRef.current;
    if (current.busy) {
      if (action) pendingActionRef.current = action;
      return;
    }
    commitText();
    commitProperty();
    pendingActionRef.current = action || null;
    if (!scenesEqual(current.scene, current.saved)) {
      current.confirmLeave = true;
      notify();
    } else exit();
  }, [commitText, commitProperty, exit, notify]);

  const toggleEditing = useCallback(() => {
    const current = stateRef.current;
    if (current.busy || !optionsRef.current.enabled || !optionsRef.current.image) return;
    if (current.editing) requestLeave();
    else {
      current.editing = true;
      current.tool = 'select';
      current.error = null;
      current.savedNotice = false;
      notify();
    }
  }, [requestLeave, notify]);

  const save = useCallback(async (leave = true) => {
    const current = stateRef.current;
    if (current.busy || !optionsRef.current.enabled) return false;
    commitText();
    commitProperty();
    if (scenesEqual(current.scene, current.saved)) {
      if (leave) exit();
      return true;
    }
    const savedScene = cloneScene(current.scene);
    const { image: original, sourceImage, screenshotId: id, onSaved: savedCallback } = optionsRef.current;
    current.busy = true;
    current.error = null;
    current.selectedId = null;
    notify();
    try {
      const canvas = document.createElement('canvas');
      renderScene(canvas, original, savedScene);
      const imageSrc = canvas.toDataURL('image/png');
      const imageData = imageSrc.split(',')[1];
      if (!imageData) throw new Error('Unable to encode the edited image');
      const result = await saveEditedScreenshot(id, current.imageHash, imageData);
      if (!mountedRef.current) return true;
      // Keep the saved raster visible while its replacement image decodes.
      current.baseImage = canvas;
      current.baseSource = sourceImage;
      current.scene = [];
      current.saved = [];
      current.undo = [];
      current.redo = [];
      current.imageHash = result.image_hash;
      current.savedNotice = true;
      current.confirmLeave = false;
      current.tool = 'select';
      // Refreshing the surrounding view must not turn a successful durable save
      // into a reported failure or restore the old dirty state.
      try {
        Promise.resolve(savedCallback?.(result, imageSrc)).catch((error) => {
          console.warn('Failed to refresh the saved snapshot:', error);
        });
      }
      catch (error) { console.warn('Failed to refresh the saved snapshot:', error); }
      current.busy = false;
      if (leave) exit();
      else notify();
      return true;
    } catch (error) {
      if (!mountedRef.current) return false;
      current.busy = false;
      current.error = String(error);
      notify();
      return false;
    }
  }, [commitText, commitProperty, exit, notify]);

  const discard = useCallback(() => {
    const current = stateRef.current;
    if (current.busy) return;
    current.scene = cloneScene(current.saved);
    current.undo = [];
    current.redo = [];
    current.error = null;
    exit();
  }, [exit]);

  const continueEditing = useCallback(() => {
    stateRef.current.confirmLeave = false;
    pendingActionRef.current = null;
    notify();
  }, [notify]);

  useEffect(() => {
    onNavigationGuardChange?.(state.editing || state.busy ? requestLeave : null);
    return () => onNavigationGuardChange?.(null);
  }, [onNavigationGuardChange, requestLeave, state.editing, state.busy]);

  return {
    ...state, stateRef, revision, dirty: !scenesEqual(state.scene, state.saved),
    selected: state.scene.find((node) => node.id === state.selectedId) || null,
    canEdit: Boolean(image && enabled && state.imageHash),
    publish, record, notify, measure, fit, commitText, beginText, updateText, select,
    chooseTool, removeSelected, beginProperty, setOption, commitProperty,
    undoAction: () => changeHistory(), redoAction: () => changeHistory(true),
    toggleEditing, requestLeave, save, discard, continueEditing,
  };
}
