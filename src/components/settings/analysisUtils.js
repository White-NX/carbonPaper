export const REFRESH_INTERVAL_MS = 30000;

export const formatBytes = (bytes) => {
  if (bytes === null || bytes === undefined) return '--';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / Math.pow(1024, index);
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[index]}`;
};

export const formatTimestamp = (ms) => {
  if (!ms) return '--';
  return new Date(ms).toLocaleString();
};
