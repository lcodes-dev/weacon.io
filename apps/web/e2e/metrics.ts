import type { Page } from "playwright-core";

/** Frame gaps as percentiles, and the page's heap and DOM size after a garbage collection, for `npm run measure`. */
export async function pageMetrics(page: Page, frameGaps: readonly number[]) {
  const gaps = [...frameGaps].sort((a, b) => a - b);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("HeapProfiler.collectGarbage");
  await cdp.send("Performance.enable");
  const { metrics } = await cdp.send("Performance.getMetrics");
  const metric = (name: string) => metrics.find((entry) => entry.name === name)?.value ?? 0;
  const at = (share: number) => Math.round(gaps[Math.min(gaps.length - 1, Math.floor(gaps.length * share))] ?? 0);
  return {
    frames: gaps.length,
    frameGapMs: { median: at(0.5), p95: at(0.95), p99: at(0.99), longest: Math.round(gaps.at(-1) ?? 0) },
    jsHeapMiB: Math.round(metric("JSHeapUsedSize") / 2 ** 20),
    domNodes: metric("Nodes"),
  };
}
