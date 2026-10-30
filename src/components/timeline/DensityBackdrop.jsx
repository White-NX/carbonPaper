import React, { useEffect, useRef } from 'react';
import { accentWithAlpha } from '../../lib/timeline_palette';

/** Actual screenshot density shares the activity band instead of occupying a row. */
export default function DensityBackdrop({ buckets, timeToX, width, height, overscanPx, stageRef }) {
  const canvasRef = useRef(null);
  const canvasWidth = width + overscanPx * 2;
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !canvasWidth || !height) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.ceil(canvasWidth * dpr);
    canvas.height = Math.ceil(height * dpr);
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    const maxCount = Math.max(0, ...buckets.map((bucket) => bucket.count));
    if (!maxCount) return;
    const baseline = height - 1;
    const points = [];
    let previousRight = null;
    for (const bucket of buckets) {
      const left = timeToX(bucket.timestamp) + overscanPx;
      const right = timeToX(bucket.timestamp + bucket.bucketMs) + overscanPx;
      if (right < 0 || left > canvasWidth) continue;
      if (previousRight != null && left - previousRight > 0.5) {
        points.push([previousRight, baseline], [left, baseline]);
      }
      const y = baseline - Math.sqrt(bucket.count / maxCount) * (height - 7);
      points.push([left, y], [right, y]);
      previousRight = right;
    }
    if (!points.length) return;
    ctx.beginPath();
    ctx.moveTo(points[0][0], baseline);
    for (const [x, y] of points) ctx.lineTo(x, y);
    ctx.lineTo(points[points.length - 1][0], baseline);
    ctx.closePath();
    const fill = ctx.createLinearGradient(0, 0, 0, height);
    fill.addColorStop(0, accentWithAlpha(0.22));
    fill.addColorStop(1, accentWithAlpha(0.03));
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.beginPath();
    points.forEach(([x, y], index) => index ? ctx.lineTo(x, y) : ctx.moveTo(x, y));
    ctx.strokeStyle = accentWithAlpha(0.22);
    ctx.lineWidth = 1;
    ctx.stroke();
  }, [buckets, timeToX, canvasWidth, height, overscanPx]);
  return <div ref={stageRef} className="pointer-events-none absolute inset-0" style={{ willChange: 'transform' }}>
    <canvas ref={canvasRef} className="absolute top-0" style={{ left: -overscanPx, width: canvasWidth, height }} />
  </div>;
}
