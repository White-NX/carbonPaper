import { getApiBase } from './utils';

const apiFetch = async (endpoint, options = {}) => {
  const method = options.method || 'GET';
  const headers = { ...options.headers };
  const url = `${getApiBase()}${endpoint}`;

  let body = undefined;
  if (options.data) {
    if (options.data instanceof FormData) {
      body = options.data;
    } else {
      body = JSON.stringify(options.data);
      headers['Content-Type'] = 'application/json';
    }
  }

  const res = await fetch(url, {
    method,
    headers,
    body,
  });

  if (!res.ok) {
    throw new Error(`API request failed: ${res.status} ${res.statusText}`);
  }

  const contentType = res.headers.get('content-type');
  let data;
  if (contentType && contentType.includes('application/json')) {
    data = await res.json();
  } else {
    data = await res.text();
  }

  return { data };
};

export const axios = {
  get: (url, config) => apiFetch(url, { method: 'GET', ...config }),
  post: (url, data, config) => apiFetch(url, { method: 'POST', data, ...config }),
  delete: (url, config) => apiFetch(url, { method: 'DELETE', ...config }),
};
