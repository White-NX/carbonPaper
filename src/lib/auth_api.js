/**
 * 认证管理 API - Windows Hello 会话管理
 * 
 * 用户数据在加密存储中，访问前需要 Windows Hello 认证。
 * 会话期限与前后台策略由后端管理。后台工作等待解锁；显式操作可请求认证。
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

const missingKey = (error) => /KeyNotFound|Credential key not found/i.test(String(error));

async function verifyForPrompt(mode) {
    if (mode === 'unlock') await invoke('credential_initialize');
    try {
        return await invoke('credential_verify_user');
    } catch (error) {
        if (mode !== 'unlock' || !missingKey(error)) throw error;
        // Preserve the lock screen's one initialization retry for a missing key.
        await invoke('credential_initialize');
        return invoke('credential_verify_user');
    }
}

/**
 * 请求 Windows Hello；同一 WebView 中的并发调用共享一个结果。
 * `unlock` 初始化凭据，`action` 和 `gate` 验证已有凭据。
 * @returns {Promise<boolean>} 认证是否成功
 */
export const requestAuth = async ({ mode = 'action' } = {}) => {
    if (!['unlock', 'action', 'gate'].includes(mode)) throw new Error('INVALID_AUTH_PROMPT_MODE');
    // 如果已经有认证请求在进行中，返回同一个 Promise
    if (pendingAuthRequest) {
        return pendingAuthRequest;
    }
    
    const authenticate = async () => {
        try {
            const result = await verifyForPrompt(mode);
            
            if (result) {
                // 认证成功，清除缓存
                cachedAuthStatus = true;
                lastCheckTime = Date.now();
            }
            
            if (!result) {
                cachedAuthStatus = false;
                emitAuthRequired();
            }
            return result;
        } catch (e) {
            console.error("Authentication failed", e);
            cachedAuthStatus = false;
            emitAuthRequired();
            
            // 用户取消了认证
            if (/cancelled|canceled|UserCancelled|AUTH_CANCELLED/i.test(String(e))) {
                throw new Error('AUTH_CANCELLED');
            }
            if (/WindowsHelloNotAvailable|Windows Hello is not available/i.test(String(e))) {
                throw new Error('AUTH_NOT_AVAILABLE');
            }
            
            throw e;
        }
    };
    pendingAuthRequest = authenticate().finally(() => { pendingAuthRequest = null; });
    
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
 * AUTH_REQUIRED 默认通知锁屏；显式操作可请求认证并在限额内重试。
 * 
 * @param {Function} apiCall - 要调用的 API 函数
 * @param {Object} options - 选项
 * @param {boolean} options.autoPrompt - 是否自动弹出认证对话框（默认 false）
 * @param {'defer'|'action'} options.promptMode - 明确等待解锁或为用户操作请求认证；优先于 autoPrompt
 * @param {number} options.maxRetries - 最大重试次数（默认 1）
 * @returns {Promise<any>} API 调用结果
 */
export const withAuth = async (apiCall, options = {}) => {
    const { autoPrompt = false, maxRetries = 1 } = options;
    const promptMode = options.promptMode ?? (autoPrompt ? 'action' : 'defer');
    if (!['defer', 'action'].includes(promptMode)) throw new Error('INVALID_AUTH_PROMPT_MODE');
    if (!Number.isSafeInteger(maxRetries) || maxRetries < 0) throw new Error('INVALID_AUTH_RETRY_LIMIT');
    
    for (let attempt = 0; attempt <= maxRetries; attempt++) {
        try {
            return await apiCall();
        } catch (e) {
            const errorStr = e.toString();
            
            // 检查是否是认证错误
            if (errorStr.includes('AUTH_REQUIRED')) {
                if (promptMode === 'defer') {
                    emitAuthRequired();
                    throw new Error('AUTH_REQUIRED');
                }
                
                if (attempt < maxRetries) {
                    // 请求认证
                    const authResult = await requestAuth({ mode: 'action' });
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
    const handleVisibilityChange = () => {
        const isVisible = document.visibilityState === 'visible';
        setForegroundState(isVisible);
    };
    const handleFocus = () => {
        setForegroundState(true);
    };
    const handleBlur = () => {};

    // 监听页面可见性变化
    document.addEventListener('visibilitychange', handleVisibilityChange);
    
    // 监听窗口焦点变化
    window.addEventListener('focus', handleFocus);
    
    window.addEventListener('blur', handleBlur);
    
    // 初始状态
    setForegroundState(document.visibilityState === 'visible');

    return () => {
        document.removeEventListener('visibilitychange', handleVisibilityChange);
        window.removeEventListener('focus', handleFocus);
        window.removeEventListener('blur', handleBlur);
    };
};

export default {
    checkAuthSession,
    requestAuth,
    lockSession,
    setForegroundState,
    withAuth,
    initAuthListeners,
};
