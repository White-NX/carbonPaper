/**
 * 认证管理 API - Windows Hello 会话管理
 * 
 * 用户数据在加密存储中，访问前需要 Windows Hello 认证。
 * 认证成功后，会话有效期为 15 分钟或直到应用进入后台。
 */

import { invoke } from '@tauri-apps/api/core';

// 认证状态缓存（用于减少频繁检查）
let cachedAuthStatus = null;
let lastCheckTime = 0;
const CHECK_INTERVAL = 5000; // 5秒内不重复检查

// 认证请求去重（防止多个组件同时请求认证）
let pendingAuthRequest = null;

const emitAuthRequired = () => {
    if (typeof window !== 'undefined') {
        window.dispatchEvent(new CustomEvent('cp-auth-required'));
    }
};

/**
 * 检查当前认证会话是否有效
 * @returns {Promise<boolean>} 是否已认证
 */
export const checkAuthSession = async () => {
    const now = Date.now();
    
    // 使用缓存避免频繁调用
    if (cachedAuthStatus !== null && now - lastCheckTime < CHECK_INTERVAL) {
        return cachedAuthStatus;
    }
    
    try {
        const isValid = await invoke('credential_check_session');
        cachedAuthStatus = isValid;
        lastCheckTime = now;
        return isValid;
    } catch (e) {
        console.error("Failed to check auth session", e);
        cachedAuthStatus = false;
        return false;
    }
};

/**
 * 请求用户认证（弹出 Windows Hello 对话框）
 * 会自动去重，多个调用会共享同一个认证请求
 * @returns {Promise<boolean>} 认证是否成功
 */
export const requestAuth = async () => {
    // 如果已经有认证请求在进行中，返回同一个 Promise
    if (pendingAuthRequest) {
        return pendingAuthRequest;
    }
    
    pendingAuthRequest = (async () => {
        try {
            const result = await invoke('credential_verify_user');
            
            if (result) {
                // 认证成功，清除缓存
                cachedAuthStatus = true;
                lastCheckTime = Date.now();
            }
            
            return result;
        } catch (e) {
            console.error("Authentication failed", e);
            cachedAuthStatus = false;
            
            // 用户取消了认证
            if (e.toString().includes('cancelled') || e.toString().includes('UserCancelled')) {
                throw new Error('AUTH_CANCELLED');
            }
            
            throw e;
        } finally {
            pendingAuthRequest = null;
        }
    })();
    
    return pendingAuthRequest;
};

/**
 * 手动锁定会话（用户可以主动锁定）
 */
export const lockSession = async () => {
    try {
        await invoke('credential_lock_session');
        cachedAuthStatus = false;
    } catch (e) {
        console.error("Failed to lock session", e);
    }
};

/**
 * 通知后端应用的前台/后台状态
 * @param {boolean} inForeground - 是否在前台
 */
export const setForegroundState = async (inForeground) => {
    try {
        await invoke('credential_set_foreground', { inForeground });
        
        // 如果进入后台，清除本地缓存
        if (!inForeground) {
            cachedAuthStatus = null;
        }
    } catch (e) {
        console.error("Failed to set foreground state", e);
    }
};

/**
 * 包装器：自动处理认证的 API 调用
 * 如果返回 AUTH_REQUIRED 错误，自动请求认证并重试
 * 
 * @param {Function} apiCall - 要调用的 API 函数
 * @param {Object} options - 选项
 * @param {boolean} options.autoPrompt - 是否自动弹出认证对话框（默认 false）
 * @param {number} options.maxRetries - 最大重试次数（默认 1）
 * @returns {Promise<any>} API 调用结果
 */
export const withAuth = async (apiCall, options = {}) => {
    const { autoPrompt = false, maxRetries = 1 } = options;
    
    for (let attempt = 0; attempt <= maxRetries; attempt++) {
        try {
            return await apiCall();
        } catch (e) {
            const errorStr = e.toString();
            
            // 检查是否是认证错误
            if (errorStr.includes('AUTH_REQUIRED')) {
                if (!autoPrompt) {
                    emitAuthRequired();
                    throw new Error('AUTH_REQUIRED');
                }
                
                if (attempt < maxRetries) {
                    // 请求认证
                    const authResult = await requestAuth();
                    if (authResult) {
                        // 认证成功，重试 API 调用
                        continue;
                    }
                }
                
                emitAuthRequired();
                throw new Error('AUTH_REQUIRED');
            }
            
            // 其他错误直接抛出
            throw e;
        }
    }
};

/**
 * 初始化认证监听器（监听窗口可见性变化）
 * 应在应用启动时调用
 */
export const initAuthListeners = () => {
    // 监听页面可见性变化
    document.addEventListener('visibilitychange', () => {
        const isVisible = document.visibilityState === 'visible';
        setForegroundState(isVisible);
    });
    
    // 监听窗口焦点变化
    window.addEventListener('focus', () => {
        setForegroundState(true);
    });
    
    window.addEventListener('blur', () => {
        // 失去焦点不立即设为后台，让用户切换应用时有时间
        // 只有当窗口完全不可见时才设为后台（由 visibilitychange 处理）
    });
    
    // 初始状态
    setForegroundState(document.visibilityState === 'visible');
};

export default {
    checkAuthSession,
    requestAuth,
    lockSession,
    setForegroundState,
    withAuth,
    initAuthListeners,
};
