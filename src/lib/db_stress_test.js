/**
 * DB Query Stress Test Module
 *
 * Simulates the rapid timeline queries that occur during Alt+Tab scenarios.
 * Sends concurrent get_screenshots_by_time_range requests to reproduce
 * the mutex contention and pipe busy conditions seen in production logs.
 *
 * Usage: Import and call from browser console, or add a temporary button.
 *   import { runStressTest } from './lib/db_stress_test';
 *   runStressTest();
 */

import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

// Default config
const DEFAULT_CONFIG = {
    // Time range: query the last N seconds of screenshots
    rangeSeconds: 180,
    // Total number of queries to send
    totalQueries: 30,
    // How many queries to send per batch (concurrent)
    batchSize: 5,
    // Delay between batches (ms)
    batchDelayMs: 200,
    // Whether to also interleave density queries
    includeDensity: true,
    // Density bucket size (ms)
    densityBucketMs: 10000,
    // Log level: 'quiet' | 'normal' | 'verbose'
    logLevel: 'normal',
};

function log(config, level, ...args) {
    if (config.logLevel === 'quiet') return;
    if (level === 'verbose' && config.logLevel !== 'verbose') return;
    console.log('[StressTest]', ...args);
}

async function singleTimelineQuery(startTime, endTime) {
    const t0 = performance.now();
    try {
        const records = await invoke('storage_get_timeline', {
            startTime,
            endTime,
        });
        const elapsed = performance.now() - t0;
        return { ok: true, elapsed, count: records?.length || 0 };
    } catch (e) {
        const elapsed = performance.now() - t0;
        return { ok: false, elapsed, error: String(e) };
    }
}

async function singleDensityQuery(startTime, endTime, bucketMs) {
    const t0 = performance.now();
    try {
        const buckets = await invoke('storage_get_timeline_density', {
            startTime,
            endTime,
            bucketMs,
        });
        const elapsed = performance.now() - t0;
        return { ok: true, elapsed, count: buckets?.length || 0 };
    } catch (e) {
        const elapsed = performance.now() - t0;
        return { ok: false, elapsed, error: String(e) };
    }
}

/**
 * Run the stress test.
 * @param {Partial<typeof DEFAULT_CONFIG>} userConfig
 */
export async function runStressTest(userConfig = {}) {
    const config = { ...DEFAULT_CONFIG, ...userConfig };

    console.group('%c[DB Stress Test] Starting', 'color: #ff6b6b; font-weight: bold');
    console.log('Config:', config);

    // Ensure auth is valid before starting
    try {
        await withAuth(async () => {
            await invoke('credential_check_session');
        });
    } catch (e) {
        console.error('[StressTest] Auth required. Please authenticate first.');
        console.groupEnd();
        return null;
    }

    const now = Date.now();
    const startTime = now - config.rangeSeconds * 1000;
    const endTime = now;

    log(config, 'normal',
        `Querying range: ${new Date(startTime).toLocaleTimeString()} ~ ${new Date(endTime).toLocaleTimeString()}`
    );

    const results = [];
    let queryIndex = 0;
    let failCount = 0;
    let pipeErrors = 0;
    const testStart = performance.now();

    for (let batch = 0; queryIndex < config.totalQueries; batch++) {
        const batchPromises = [];
        const batchStart = queryIndex;

        for (let i = 0; i < config.batchSize && queryIndex < config.totalQueries; i++, queryIndex++) {
            const idx = queryIndex;

            // Timeline query
            batchPromises.push(
                singleTimelineQuery(startTime, endTime).then(r => {
                    r.type = 'timeline';
                    r.index = idx;
                    return r;
                })
            );

            // Optionally interleave density queries
            if (config.includeDensity && i % 2 === 0) {
                batchPromises.push(
                    singleDensityQuery(startTime, endTime, config.densityBucketMs).then(r => {
                        r.type = 'density';
                        r.index = idx;
                        return r;
                    })
                );
            }
        }

        log(config, 'verbose', `Batch ${batch}: sending ${batchPromises.length} queries (${batchStart}-${queryIndex - 1})`);

        const batchResults = await Promise.all(batchPromises);

        for (const r of batchResults) {
            results.push(r);
            if (!r.ok) {
                failCount++;
                if (r.error && r.error.includes('231')) {
                    pipeErrors++;
                }
                log(config, 'normal', `FAIL [${r.type}#${r.index}] ${r.elapsed.toFixed(0)}ms: ${r.error}`);
            } else {
                log(config, 'verbose', `OK [${r.type}#${r.index}] ${r.elapsed.toFixed(0)}ms, ${r.count} records`);
            }
        }

        // Delay between batches
        if (queryIndex < config.totalQueries && config.batchDelayMs > 0) {
            await new Promise(r => setTimeout(r, config.batchDelayMs));
        }
    }

    const totalElapsed = performance.now() - testStart;

    // Compute stats
    const okResults = results.filter(r => r.ok);
    const elapsedValues = okResults.map(r => r.elapsed).sort((a, b) => a - b);

    const stats = {
        totalQueries: results.length,
        succeeded: okResults.length,
        failed: failCount,
        pipeErrors,
        totalTimeMs: Math.round(totalElapsed),
        avgMs: elapsedValues.length > 0 ? Math.round(elapsedValues.reduce((a, b) => a + b, 0) / elapsedValues.length) : 0,
        p50Ms: elapsedValues.length > 0 ? Math.round(elapsedValues[Math.floor(elapsedValues.length * 0.5)]) : 0,
        p95Ms: elapsedValues.length > 0 ? Math.round(elapsedValues[Math.floor(elapsedValues.length * 0.95)]) : 0,
        p99Ms: elapsedValues.length > 0 ? Math.round(elapsedValues[Math.floor(elapsedValues.length * 0.99)]) : 0,
        maxMs: elapsedValues.length > 0 ? Math.round(elapsedValues[elapsedValues.length - 1]) : 0,
    };

    console.log('%c[Results]', 'color: #51cf66; font-weight: bold');
    console.table(stats);

    if (pipeErrors > 0) {
        console.warn(`%c${pipeErrors} pipe busy errors (os error 231) detected!`, 'color: #ff6b6b');
    }

    // Show slow queries (>2s)
    const slowQueries = results.filter(r => r.elapsed > 2000);
    if (slowQueries.length > 0) {
        console.log(`%c${slowQueries.length} slow queries (>2s):`, 'color: #ffd43b');
        console.table(slowQueries.map(r => ({
            type: r.type,
            index: r.index,
            elapsed: `${r.elapsed.toFixed(0)}ms`,
            ok: r.ok,
            error: r.error || '',
        })));
    }

    console.groupEnd();
    return { stats, results };
}

/**
 * Run with aggressive settings to maximize contention.
 */
export async function runAggressiveTest() {
    return runStressTest({
        totalQueries: 60,
        batchSize: 10,
        batchDelayMs: 50,
        includeDensity: true,
        logLevel: 'normal',
    });
}

/**
 * Run a sustained test over a longer period, simulating realistic polling.
 * Sends queries in waves every intervalMs for durationSeconds.
 */
export async function runSustainedTest(durationSeconds = 30, intervalMs = 3000) {
    console.group('%c[Sustained DB Stress Test]', 'color: #ff6b6b; font-weight: bold');
    console.log(`Duration: ${durationSeconds}s, interval: ${intervalMs}ms`);

    const allResults = [];
    const testStart = performance.now();
    let wave = 0;

    while ((performance.now() - testStart) < durationSeconds * 1000) {
        wave++;
        const now = Date.now();
        const startTime = now - 180 * 1000;
        const endTime = now;

        // Simulate what the frontend does: timeline + density queries concurrently
        const promises = [
            singleTimelineQuery(startTime, endTime).then(r => ({ ...r, type: 'timeline', wave })),
            singleDensityQuery(startTime, endTime, 10000).then(r => ({ ...r, type: 'density', wave })),
        ];

        const waveResults = await Promise.all(promises);

        for (const r of waveResults) {
            allResults.push(r);
            const status = r.ok ? 'OK' : 'FAIL';
            const detail = r.ok ? `${r.count} records` : r.error;
            console.log(
                `[Wave ${wave}] ${r.type}: ${status} ${r.elapsed.toFixed(0)}ms (${detail})`
            );
        }

        // Wait for next interval
        const elapsed = performance.now() - testStart;
        const nextWaveAt = wave * intervalMs;
        if (nextWaveAt > elapsed) {
            await new Promise(r => setTimeout(r, nextWaveAt - elapsed));
        }
    }

    const okResults = allResults.filter(r => r.ok);
    const failResults = allResults.filter(r => !r.ok);
    const pipeErrors = failResults.filter(r => r.error?.includes('231')).length;
    const elapsedValues = okResults.map(r => r.elapsed).sort((a, b) => a - b);

    console.log('%c[Sustained Test Results]', 'color: #51cf66; font-weight: bold');
    console.table({
        waves: wave,
        totalQueries: allResults.length,
        succeeded: okResults.length,
        failed: failResults.length,
        pipeErrors,
        avgMs: elapsedValues.length > 0 ? Math.round(elapsedValues.reduce((a, b) => a + b, 0) / elapsedValues.length) : 0,
        p95Ms: elapsedValues.length > 0 ? Math.round(elapsedValues[Math.floor(elapsedValues.length * 0.95)]) : 0,
        maxMs: elapsedValues.length > 0 ? Math.round(elapsedValues[elapsedValues.length - 1]) : 0,
    });

    console.groupEnd();
    return allResults;
}

// Expose to window for console access
if (typeof window !== 'undefined') {
    window.__dbStressTest = {
        run: runStressTest,
        aggressive: runAggressiveTest,
        sustained: runSustainedTest,
    };
    console.log(
        '%c[DB Stress Test] Available via window.__dbStressTest.run() / .aggressive() / .sustained()',
        'color: #868e96'
    );
}
