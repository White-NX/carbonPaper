import { invoke } from '@tauri-apps/api/core';

/**
 * 切换到轻量模式（销毁窗口）
 */
export async function switchToLightweightMode() {
  return await invoke('switch_to_lightweight_mode');
}

/**
 * 切换到标准模式（重建窗口）
 */
export async function switchToStandardMode() {
  return await invoke('switch_to_standard_mode');
}

/**
 * 获取当前是否处于轻量模式
 */
export async function getLightweightStatus() {
  return await invoke('get_lightweight_status');
}

/**
 * 获取轻量模式配置
 */
export async function getLightweightConfig() {
  return await invoke('get_lightweight_config');
}

/**
 * 设置轻量模式配置
 */
export async function setLightweightConfig(config) {
  return await invoke('set_lightweight_config', { config });
}
