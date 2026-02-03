export const defaultFilterSettings = {
  processes: ['carbonpaper.exe'],
  titles: ['carbonpaper', 'pornhub'],
  ignoreProtected: true,
};

export const normalizeList = (value) =>
  value
    .split(/[\,\n]+/)
    .map((v) => v.trim())
    .filter(Boolean)
    .map((v) => v.toLowerCase());

export const formatInvokeError = (error) => {
  if (!error) return '未知错误';
  if (typeof error === 'string') return error;
  if (typeof error === 'object') {
    if (typeof error.message === 'string' && error.message.trim()) return error.message;
    try {
      return JSON.stringify(error);
    } catch (e) {
      return '未知错误';
    }
  }
  return String(error);
};
