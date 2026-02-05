import { invoke } from '@tauri-apps/api/core';
import { withAuth, requestAuth, checkAuthSession } from './auth_api';

// Re-export auth functions for convenience
export { requestAuth, checkAuthSession };
export { initAuthListeners, lockSession } from './auth_api';

// Simple request queue to limit concurrent pipe connections
class RequestQueue {
    constructor(maxConcurrent = 3) {
        this.maxConcurrent = maxConcurrent;
        this.running = 0;
        this.queue = [];
    }

    async enqueue(fn) {
        return new Promise((resolve, reject) => {
            const task = async () => {
                try {
                    const result = await fn();
                    resolve(result);
                } catch (e) {
                    reject(e);
                } finally {
                    this.running--;
                    this.processNext();
                }
            };

            this.queue.push(task);
            this.processNext();
        });
    }

    processNext() {
        while (this.running < this.maxConcurrent && this.queue.length > 0) {
            const task = this.queue.shift();
            this.running++;
            task();
        }
    }
}

// Global request queue for image fetching (limit concurrent requests)
const imageQueue = new RequestQueue(3);

/**
 * 初始化凭据管理器 - 应在应用启动时调用
 */
export const initializeCredentials = async () => {
    try {
        const result = await invoke('credential_initialize');
        return result;
    } catch (e) {
        console.error("Failed to initialize credentials", e);
        throw e;
    }
};

/**
 * 请求用户验证（Windows Hello PIN）
 * @deprecated 使用 auth_api.js 中的 requestAuth
 */
export const verifyUser = async () => {
    return requestAuth();
};

/**
 * 获取时间线数据 - 直接从 Rust 存储层获取
 * 需要认证才能访问
 */
export const getTimeline = async (startTime, endTime) => {
    return withAuth(async () => {
        // 使用新的 Rust 存储命令
        const records = await invoke('storage_get_timeline', {
            startTime: startTime,
            endTime: endTime
        });
        console.log('[Timeline] Fetched records:', records?.length || 0, 'range:', new Date(startTime).toLocaleString(), '-', new Date(endTime).toLocaleString());
        return records || [];
    });
};

/**
 * 获取图片 - 直接从 Rust 存储层获取
 * 需要认证才能访问
 */
export const fetchImage = async (id, path = null) => {
    // Use queue to limit concurrent image requests
    return imageQueue.enqueue(async () => {
        return withAuth(async () => {
            // 优先使用 Rust 存储层
            const response = await invoke('storage_get_image', { id, path });
            if (response && response.status === 'success' && response.data) {
                return `data:${response.mime_type || 'image/png'};base64,${response.data}`;
            }
            return null;
        });
    });
};

/**
 * 搜索截图
 * @param {string} query - 搜索查询
 * @param {string} mode - 'ocr' 使用 Rust 存储, 'nl' 使用 Python 自然语言搜索
 * @param {object} options - 搜索选项
 * 需要认证才能访问
 */
export const searchScreenshots = async (query, mode = 'ocr', options = {}) => {
    const {
        limit = 20,
        offset = 0,
        processNames = [],
        startTime = null,
        endTime = null,
        fuzzy = true
    } = options || {};
    
    return withAuth(async () => {
        // 自然语言搜索使用 Python IPC
        if (mode === 'nl') {
            const response = await invoke('execute_monitor_command', {
                payload: {
                    command: 'search_nl',
                    query: query,
                    limit: limit,
                    offset: offset,
                    process_names: processNames,
                    start_time: startTime,
                    end_time: endTime,
                    fuzzy: fuzzy
                }
            });
            if (response.error) {
                throw new Error(response.error);
            }
            return response.results || [];
        }
        
        // OCR/文本搜索使用 Rust 存储层
        const results = await invoke('storage_search', {
            query: query,
            limit: limit,
            offset: offset,
            fuzzy: fuzzy,
            processNames: processNames.length > 0 ? processNames : null,
            startTime: startTime,
            endTime: endTime
        });
        return results || [];
    });
};

export const listProcesses = async () => {
    try {
        const response = await invoke('execute_monitor_command', {
            payload: {
                command: 'list_processes'
            }
        });
        if (response.error) {
            console.error('List processes error:', response.error);
            return [];
        }
        return response.processes || [];
    } catch (e) {
        console.error('Failed to list processes', e);
        return [];
    }
};

export const getScreenshotDetails = async (id, path = null) => {
    try {
        // 直接使用 Rust 存储层 API
        const response = await invoke('storage_get_screenshot_details', { id });
        if (response.error) {
            console.error("Details error:", response.error);
            return { error: response.error };
        }
        return response;
    } catch (e) {
        console.error("Failed to fetch details", e);
        return { error: e.toString() };
    }
};

export const updateMonitorFilters = async (filters) => {
    try {
        const payload = {
            command: 'update_filters',
            filters
        };
        const response = await invoke('execute_monitor_command', { payload });
        if (response?.error) {
            const err = new Error(response.error);
            if (response.error === 'unknown command') {
                err.code = 'unsupported';
            }
            throw err;
        }
        return response;
    } catch (e) {
        console.error("Failed to update monitor filters", e);
        throw e;
    }
};

export const deleteScreenshot = async (screenshotId) => {
    return withAuth(async () => {
        try {
            const response = await invoke('storage_delete_screenshot', {
                screenshotId
            });
            if (response?.error) {
                throw new Error(response.error);
            }
            return response;
        } catch (e) {
            console.error("Failed to delete screenshot", e);
            throw e;
        }
    });
};

export const deleteRecordsByTimeRange = async (minutes, centerTimestamp = null) => {
    return withAuth(async () => {
        try {
            const now = centerTimestamp || Date.now();
            let startTime, endTime;
            
            if (minutes === 'today') {
                // Delete all records from today (start of day to now)
                const today = new Date();
                today.setHours(0, 0, 0, 0);
                startTime = today.getTime();
                endTime = Date.now();
            } else {
                // Delete records within the specified minutes
                const rangeMs = minutes * 60 * 1000;
                startTime = now - rangeMs;
                endTime = now;
            }
            
            const response = await invoke('storage_delete_by_time_range', {
                startTime,
                endTime
            });
            if (response?.error) {
                throw new Error(response.error);
            }
            return response;
        } catch (e) {
            console.error("Failed to delete records by time range", e);
            throw e;
        }
    });
};

// ==================== 数据迁移 API ====================

/**
 * 列出所有未加密的明文截图文件
 * @returns {Promise<string[]>} 明文文件路径列表
 */
export const listPlaintextFiles = async () => {
    try {
        const files = await invoke('storage_list_plaintext_files');
        return files || [];
    } catch (e) {
        console.error("Failed to list plaintext files", e);
        return [];
    }
};

/**
 * 迁移所有明文截图文件（加密并删除原文件）
 * 需要认证
 * @returns {Promise<{total_files: number, migrated: number, skipped: number, errors: string[]}>}
 */
export const migratePlaintextFiles = async () => {
    return withAuth(async () => {
        const result = await invoke('storage_migrate_plaintext');
        return result;
    });
};

/**
 * 删除所有明文截图文件（不迁移，直接删除）
 * 需要认证
 * @returns {Promise<{status: string, deleted_count: number}>}
 */
export const deletePlaintextFiles = async () => {
    return withAuth(async () => {
        const result = await invoke('storage_delete_plaintext');
        return result;
    });
};
