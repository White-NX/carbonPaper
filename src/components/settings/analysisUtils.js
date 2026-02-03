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

export const buildLinePath = (points) => {
  if (!points || points.length === 0) return '';
  const times = points.map((p) => p.timestamp_ms);
  const values = points.map((p) => p.rss_bytes);
  const minTime = Math.min(...times);
  const maxTime = Math.max(...times);
  const minVal = Math.min(...values);
  const maxVal = Math.max(...values);
  const spanTime = Math.max(maxTime - minTime, 1);
  const spanVal = Math.max(maxVal - minVal, 1);

  return points
    .map((p, index) => {
      const x = ((p.timestamp_ms - minTime) / spanTime) * 100;
      const y = 100 - ((p.rss_bytes - minVal) / spanVal) * 100;
      return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
};
