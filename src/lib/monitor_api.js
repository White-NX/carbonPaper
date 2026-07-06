import { invoke } from '@tauri-apps/api/core';
import { withAuth, requestAuth, checkAuthSession } from './auth_api';

// Re-export auth functions for convenience
export { requestAuth, checkAuthSession };
export { initAuthListeners, lockSession } from './auth_api';

export const REQUEST_DEADLINES = Object.freeze({
    imageMs: 15_000,
    thumbnailMs: 15_000,
    timelineImageMs: 15_000,
    detailMs: 15_000,
});

const createDeadlineError = (deadlineMs) => {
    const err = new Error(`deadline exceeded after ${deadlineMs}ms`);
    err.code = 'deadline_exceeded';
    return err;
};

// Simple request queue to limit concurrent pipe connections
export class RequestQueue {
    constructor(maxConcurrent = 3, maxPending = 200) {
        this.maxConcurrent = maxConcurrent;
        this.maxPending = maxPending;
        this.running = 0;
        this.queue = [];
        this.pendingByKey = new Map();
        this.runningByKey = new Map();
    }

    async enqueue(fn, options = {}) {
        const { priority = 'normal', key = null, dedupe = true, deadlineMs = null } = options;

        if (dedupe && key !== null && key !== undefined) {
            const existing = this.pendingByKey.get(key) || this.runningByKey.get(key);
            if (existing) {
                if (priority === 'high' && existing.priority !== 'high') {
                    existing.priority = 'high';
                    if (this.queue.includes(existing)) {
                        this.queue = this.queue.filter((item) => item !== existing);
                        this.queue.unshift(existing);
                    }
                }
                return existing.promise;
            }
        }

        let settled = false;
        let state = 'pending';
        let deadlineTimer = null;
        let resolveRef;
        let rejectRef;
        const promise = new Promise((resolve, reject) => {
            resolveRef = resolve;
            rejectRef = reject;
        });

        const settle = (kind, value) => {
            if (settled) return;
            settled = true;
            if (deadlineTimer) {
                clearTimeout(deadlineTimer);
                deadlineTimer = null;
            }

            const wasRunning = state === 'running';
            state = 'settled';
            this.queue = this.queue.filter((item) => item !== entry);
            if (entry.key !== null && entry.key !== undefined) {
                this.pendingByKey.delete(entry.key);
                this.runningByKey.delete(entry.key);
            }
            if (wasRunning) {
                this.running = Math.max(0, this.running - 1);
            }

            if (kind === 'resolve') {
                resolveRef(value);
            } else {
                rejectRef(value);
            }

            if (wasRunning) {
                this.processNext();
            }
        };

        const entry = {
            key,
            priority,
            cancelled: false,
            promise,
            run: async () => {
                if (settled) return;
                state = 'running';
                if (entry.key !== null && entry.key !== undefined) {
                    this.pendingByKey.delete(entry.key);
                    this.runningByKey.set(entry.key, entry);
                }
                if (entry.cancelled) {
                    settle('reject', new Error('cancelled'));
                    return;
                }
                try {
                    const result = await fn();
                    settle('resolve', result);
                } catch (e) {
                    settle('reject', e);
                }
            },
            cancel: () => {
                entry.cancelled = true;
                settle('reject', new Error('cancelled'));
            }
        };

        if (Number.isFinite(deadlineMs) && deadlineMs > 0) {
            deadlineTimer = setTimeout(() => {
                entry.cancelled = true;
                settle('reject', createDeadlineError(deadlineMs));
            }, deadlineMs);
        }

        if (this.queue.length >= this.maxPending) {
            const dropped = priority === 'high' ? this.queue.pop() : this.queue.shift();
            if (dropped) dropped.cancel();
        }

        if (priority === 'high') {
            this.queue.unshift(entry);
        } else {
            this.queue.push(entry);
        }
        if (entry.key !== null && entry.key !== undefined) {
            this.pendingByKey.set(entry.key, entry);
        }
        this.processNext();
        return promise;
    }

    clearPending() {
        this.queue.forEach((entry) => entry.cancel());
        this.queue = [];
        this.pendingByKey.clear();
    }

    cancelByKey(key) {
        if (key === null || key === undefined) return false;
        const entry = this.pendingByKey.get(key) || this.runningByKey.get(key);
        if (!entry) return false;
        this.queue = this.queue.filter((item) => item !== entry);
        entry.cancel();
        return true;
    }

    processNext() {
        while (this.running < this.maxConcurrent && this.queue.length > 0) {
            const task = this.queue.shift();
            this.running++;
            task.run();
        }
    }
}

// Global request queue for image fetching (limit concurrent requests)
const imageQueue = new RequestQueue(3, 100);
// Dedicated queue for card thumbnails — separate from imageQueue to avoid starvation
const thumbnailQueue = new RequestQueue(6, 100);
// Timeline thumbnails should load in parallel to avoid long UI delays after pan/zoom
const timelineImageQueue = new RequestQueue(20, 800);
const detailQueue = new RequestQueue(3, 100);

const imageRequestKey = (prefix, id, path) => {
    if (id !== null && id !== undefined && id !== '') return `${prefix}:id:${id}`;
    if (path) return `${prefix}:path:${path}`;
    return null;
};

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
export const getTimeline = async (startTime, endTime, maxRecords = null) => {
    return withAuth(async () => {
        // 使用新的 Rust 存储命令
        const params = {
            startTime: startTime,
            endTime: endTime
        };
        if (maxRecords !== null) {
            params.maxRecords = maxRecords;
        }
        const records = await invoke('storage_get_timeline', params);
        return records || [];
    });
};

/**
 * 获取时间线密度数据 - 返回按时间桶分组的快照计数
 * 用于大时间尺度下显示快照密集程度
 */
export const getTimelineDensity = async (startTime, endTime, bucketMs) => {
    return withAuth(async () => {
        const buckets = await invoke('storage_get_timeline_density', {
            startTime,
            endTime,
            bucketMs,
        });
        return buckets || [];
    });
};

/**
 * 获取图片 - 直接从 Rust 存储层获取
 * 需要认证才能访问
 */
export const fetchImage = async (id, path = null) => {
    const key = imageRequestKey('image', id, path);
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
    }, { priority: 'high', key, deadlineMs: REQUEST_DEADLINES.imageMs });
};

/**
 * 时间线缩略图专用获取（低优先级，使用 thumbnail API）
 */
export const fetchTimelineImage = async (id, path = null, options = {}) => {
    const { priority = 'normal', key = null, deadlineMs = REQUEST_DEADLINES.timelineImageMs } = options || {};
    return timelineImageQueue.enqueue(async () => {
        return withAuth(async () => {
            try {
                const response = await invoke('storage_get_thumbnail', { id, path });
                if (response && response.status === 'success' && response.data) {
                    return `data:${response.mime_type || 'image/jpeg'};base64,${response.data}`;
                }
                return null;
            } catch (err) {
                const message = err?.toString?.() || String(err);
                if (message.includes('Image not found')) {
                    const notFound = new Error('not_found');
                    notFound.code = 'not_found';
                    throw notFound;
                }
                throw err;
            }
        });
    }, { priority, key, deadlineMs });
};

export const clearTimelineImageQueue = () => {
    timelineImageQueue.clearPending();
};

/**
 * 通用缩略图获取（用于搜索结果等卡片展示）
 */
export const fetchThumbnail = async (id, path = null) => {
    const key = imageRequestKey('thumb', id, path);
    return thumbnailQueue.enqueue(async () => {
        return withAuth(async () => {
            const response = await invoke('storage_get_thumbnail', { id, path });
            if (response && response.status === 'success' && response.data) {
                return `data:${response.mime_type || 'image/jpeg'};base64,${response.data}`;
            }
            return null;
        });
    }, { priority: 'normal', key, deadlineMs: REQUEST_DEADLINES.thumbnailMs });
};

/**
 * 批量获取缩略图（单次 IPC 往返）
 * @param {number[]} ids - 截图 ID 列表
 * @returns {Promise<Object>} { [id]: dataUrl }
 */
export const fetchThumbnailBatch = async (ids) => {
    return withAuth(async () => {
        const response = await invoke('storage_batch_get_thumbnails', { ids });
        if (!response?.results) return {};
        const mapped = {};
        for (const [id, entry] of Object.entries(response.results)) {
            if (entry.status === 'success' && entry.data) {
                mapped[id] = `data:${entry.mime_type || 'image/jpeg'};base64,${entry.data}`;
            }
        }
        return mapped;
    });
};

export const cancelTimelineImageRequest = (key) => {
    timelineImageQueue.cancelByKey(key);
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
        categories = [],
        startTime = null,
        endTime = null,
        fuzzy = true
    } = options || {};
    
    return withAuth(async () => {
        // 自然语言搜索使用 Python IPC
        if (mode === 'nl') {
            const response = await invoke('monitor_search_nl', {
                query,
                limit,
                offset,
                processNames,
                startTime,
                endTime,
                fuzzy,
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
            categories: categories.length > 0 ? categories : null,
            startTime: startTime,
            endTime: endTime
        });
        return results || [];
    });
};

export const listProcesses = async () => {
    try {
        return await withAuth(async () => {
            const processes = await invoke('storage_list_processes');
            return processes || [];
        });
    } catch (e) {
        console.error('Failed to list processes', e);
        return [];
    }
};

export const getProcessStorageStats = async () => {
    try {
        return await withAuth(async () => {
            const stats = await invoke('storage_get_process_stats');
            return stats || [];
        });
    } catch (e) {
        console.error('Failed to get process storage stats', e);
        return [];
    }
};

export const getProcessMonthlyThumbnails = async (processName, page = 0, pageSize = 60) => {
    return withAuth(async () => {
        const response = await invoke('storage_get_process_monthly_thumbnails', {
            processName,
            page,
            pageSize,
        });
        return response || null;
    });
};

export const softDeleteProcessMonth = async (processName, month = null) => {
    return withAuth(async () => {
        const response = await invoke('storage_soft_delete', {
            processName,
            month,
        });
        return response;
    });
};

export const softDeleteScreenshots = async (screenshotIds = []) => {
    return withAuth(async () => {
        const response = await invoke('storage_soft_delete_screenshots', {
            screenshotIds,
        });
        return response;
    });
};

export const getSoftDeleteQueueStatus = async () => {
    try {
        return await withAuth(async () => {
            const status = await invoke('storage_get_delete_queue_status');
            return status || { pending_screenshots: 0, pending_ocr: 0, running: false };
        });
    } catch (e) {
        console.warn('Failed to get soft delete queue status', e);
        return { pending_screenshots: 0, pending_ocr: 0, running: false };
    }
};

export const getScreenshotDetails = async (id, path = null) => {
    const key = imageRequestKey('detail', id, path);
    try {
        const response = await detailQueue.enqueue(
            () => withAuth(() => invoke('storage_get_screenshot_details', { id, path })),
            { priority: 'high', key, deadlineMs: REQUEST_DEADLINES.detailMs }
        );
        if (response?.error) {
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
    return withAuth(async () => {
        try {
            const response = await invoke('monitor_update_filters', { filters });
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
    }, { autoPrompt: true });
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
    }, { autoPrompt: true });
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
    }, { autoPrompt: true });
};

// ==================== 数据迁移 API ====================

/**
 * 列出所有未加密的明文截图文件
 * @returns {Promise<string[]>} 明文文件路径列表
 */
export const listPlaintextFiles = async () => {
    try {
        const files = await withAuth(() => invoke('storage_list_plaintext_files'));
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
 * @returns {Promise<{status: string, deleted: number}>}
 */
export const deletePlaintextFiles = async () => {
    return withAuth(async () => {
        const result = await invoke('storage_delete_plaintext');
        return result;
    });
};

export const computeLinkScores = async (links) => {
    return await invoke('storage_compute_link_scores', { links });
};

// ==================== 分类相关 ====================

export const updateScreenshotCategory = async (screenshotId, category) => {
    return await withAuth(() => invoke('storage_update_category', {
        screenshotId,
        category,
    }), { autoPrompt: true });
};

export const getCategories = async () => {
    return await withAuth(() => invoke('storage_get_categories'));
};

export const getCategoriesFromDb = async () => {
    try {
        return await withAuth(() => invoke('storage_get_categories_from_db'));
    } catch (e) {
        console.error('Failed to get categories from db', e);
        return [];
    }
};

export const batchGetCategories = async (imageHashes) => {
    try {
        return await withAuth(() => invoke('storage_batch_get_categories', { imageHashes }));
    } catch (e) {
        console.error('Failed to batch get categories', e);
        return {};
    }
};

export const classifyDebug = async ({ title = '', ocrText = '', processName = '' } = {}) => {
    const response = await withAuth(() => invoke('monitor_classify_debug', {
        title,
        ocrText,
        processName,
    }), { autoPrompt: true });
    if (response?.error) {
        throw new Error(response.error);
    }
    return response;
};

export const removeLocalAnchorsByProcess = async (category, processName) => {
    const response = await withAuth(() => invoke('monitor_remove_local_anchors_by_process', {
        category,
        processName,
    }), { autoPrompt: true });
    if (response?.error) {
        throw new Error(response.error);
    }
    return response;
};

export const getSmartClusterWorkerStatus = async () => {
    return withAuth(async () => {
        try {
            const response = await invoke('monitor_smart_cluster_worker_status');
            if (response?.error) {
                return { pending_count: 0, running: false };
            }
            return {
                pending_count: response.pending_count || 0,
                running: !!response.is_running,
                forceRunning: !!response.is_force_running,
            };
        } catch {
            return { pending_count: 0, running: false };
        }
    });
};

export const getIndexHealth = async ({ refreshVector = false } = {}) => {
    return withAuth(
        () => invoke('storage_get_index_health', { refreshVector }),
        { autoPrompt: true },
    );
};

export const retryVectorIndexing = async (limit = 32) => {
    return withAuth(
        () => invoke('storage_retry_vector_indexing', { limit }),
        { autoPrompt: true },
    );
};
