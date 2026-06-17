import { useCallback, useEffect, useRef } from 'react';
import { runClustering, saveClusteringResults } from '../lib/task_api';

const DEFAULT_DELAY_MS = 60_000;

function buildTaskRequests(clusters) {
  return clusters.map((cluster) => ({
    label: cluster.label || null,
    layer: 'hot',
    centroid: cluster.centroid || [],
    screenshot_ids: cluster.screenshot_ids || [],
    start_time: cluster.start_time || null,
    end_time: cluster.end_time || null,
    dominant_process: cluster.dominant_process || null,
    dominant_category: cluster.dominant_category || null,
  }));
}

export function useDelayedClusteringSetupRunner({
  delayMs = DEFAULT_DELAY_MS,
  onClose,
  pushNotification,
}) {
  const timerRef = useRef(null);

  useEffect(() => () => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  return useCallback((shouldRun) => {
    onClose?.();
    if (!shouldRun) return;

    if (timerRef.current) {
      clearTimeout(timerRef.current);
    }

    timerRef.current = setTimeout(async () => {
      timerRef.current = null;
      pushNotification({
        id: `clustering-start-${Date.now()}`,
        type: 'info',
        title: '任务聚类',
        message: '正在对历史快照进行任务聚类，这可能需要几分钟时间…',
        timestamp: Date.now(),
      });

      try {
        const result = await runClustering();

        if (result?.status === 'already_running') {
          pushNotification({
            id: `clustering-running-${Date.now()}`,
            type: 'info',
            title: '任务聚类',
            message: '聚类任务已在后台运行中，请稍后查看结果。',
            timestamp: Date.now(),
          });
          return;
        }

        if (result?.clusters?.length > 0) {
          const tasks = buildTaskRequests(result.clusters);
          await saveClusteringResults(tasks);
          pushNotification({
            id: `clustering-done-${Date.now()}`,
            type: 'success',
            title: '任务聚类完成',
            message: `已将历史快照归纳为 ${tasks.length} 个任务，可在"任务"面板中查看。`,
            timestamp: Date.now(),
          });
          return;
        }

        pushNotification({
          id: `clustering-empty-${Date.now()}`,
          type: 'info',
          title: '任务聚类完成',
          message: '未发现可归类的任务。快照数量可能不足，系统将在积累更多数据后自动尝试。',
          timestamp: Date.now(),
        });
      } catch (err) {
        console.error('Background clustering failed:', err);
        pushNotification({
          id: `clustering-error-${Date.now()}`,
          type: 'error',
          title: '任务聚类失败',
          message: typeof err === 'string' ? err : (err?.message || '聚类过程中发生错误，请稍后在"任务"面板手动重试。'),
          details: typeof err === 'string' ? '' : (err?.stack || ''),
          timestamp: Date.now(),
        });
      }
    }, delayMs);
  }, [delayMs, onClose, pushNotification]);
}
