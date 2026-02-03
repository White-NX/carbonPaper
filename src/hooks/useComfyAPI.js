// src/hooks/useComfyAPI.js
import { useState, useCallback } from 'react';

// === Tauri 兼容层开始 ===
// 移除所有 API 调用，仅保留空壳函数或占位数据
export const useComfyAPI = () => {
  const [history, setHistory] = useState([
    {
      promptId: 'mock-1',
      status: 'completed',
      image: { filename: 'placeholder.png', dataUrl: 'https://placehold.co/600x400' },
      workflow: 'zimage'
    }
  ]);
  const [activeTasks, setActiveTasks] = useState({});
  const [systemStats, setSystemStats] = useState({ 
    system: { gpu: { vram_used: 0, vram_total: 24576 } } 
  });
  const [workflow, setWorkflowState] = useState('zimage');

  const setWorkflow = async (newWorkflow) => {
    setWorkflowState(newWorkflow);
  };

  const fetchHistory = useCallback(async () => {
    return history;
  }, [history]);

  const generate = async () => {
    // No-op
  };

  const uploadImage = async (file) => {
    // No-op
    return { name: file.name, subfolder: '', type: 'input' };
  };

  const purgeVRAM = async () => {
    return {};
  };

  const deleteTask = async (promptId) => {
      setHistory(prev => prev.filter(task => task.promptId !== promptId));
  };

  const cancelBatch = async () => {
    return {};
  };

  const fetchModeration = async () => {
    return null;
  };

  const moderateAll = async () => {
    return { success: true, count: 0 };
  };

  const getPresets = async () => {
    return [];
  };

  const addPreset = async () => {
    return {};
  };

  const deletePreset = async () => {
    // No-op
  };

  return { 
    history, 
    activeTasks, 
    systemStats, 
    generate, 
    uploadImage, 
    purgeVRAM, 
    deleteTask, 
    cancelBatch, 
    fetchModeration, 
    moderateAll, 
    refreshHistory: fetchHistory, 
    workflow, 
    setWorkflow,
    getPresets,
    addPreset,
    deletePreset
  };
};
