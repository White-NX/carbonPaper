import { invoke } from '@tauri-apps/api/core';

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

export const getTimeline = async (startTime, endTime) => {
    try {
        const response = await invoke('execute_monitor_command', {
            payload: {
                command: 'get_timeline',
                start_time: startTime,
                end_time: endTime
            }
        });
        if (response.error) {
            throw new Error(response.error);
        }
        return response.records || [];
    } catch (e) {
        console.error("Failed to get timeline", e);
        return [];
    }
};

export const fetchImage = async (id, path = null) => {
    // Use queue to limit concurrent image requests
    return imageQueue.enqueue(async () => {
        try {
            const payload = { command: 'get_image' };
            if (id !== null && id !== undefined) payload.id = id;
            if (path) payload.path = path;

            const response = await invoke('execute_monitor_command', { payload });
            if (response.error) {
                console.error("Image error:", response.error);
                return null;
            }
            if (response.data) {
                return `data:${response.mime_type || 'image/png'};base64,${response.data}`;
            }
            return null;
        } catch (e) {
            console.error("Failed to fetch image", e);
            return null;
        }
    });
};

export const searchScreenshots = async (query, mode = 'ocr', options = {}) => {
    const command = mode === 'nl' ? 'search_nl' : 'search';
    const {
        limit = 20,
        offset = 0,
        processNames = [],
        startTime = null,
        endTime = null,
        fuzzy = true
    } = options || {};
    try {
        const response = await invoke('execute_monitor_command', {
            payload: {
                command: command,
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
             console.error("Search error:", response.error);
             return [];
        }
        return response.results || [];
    } catch (e) {
        console.error("Failed to search", e);
        return [];
    }
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
        const payload = { command: 'get_screenshot_details' };
        if (id !== null && id !== undefined) payload.id = id;
        if (path) payload.path = path;

        const response = await invoke('execute_monitor_command', { payload });
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
