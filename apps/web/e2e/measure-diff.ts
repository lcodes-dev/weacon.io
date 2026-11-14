// The diff half of `npm run measure`: opens the fake server's 5,320-line change in the built page
// in Chromium, scrolls it top to bottom, and prints frame gaps, memory and DOM size as JSON.

import { fakeServer, start } from "./harness.ts";
import { pageMetrics } from "./metrics.ts";

// Pixels scrolled each frame, about a screen a second at 60 frames a second.
const STEP_PX = 15;

const harness = await start(fakeServer(1_000, 0), { query: "&workspace=w-1&chat=c-1" });
try {
  const { page } = harness;
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.getByText("Connected").waitFor();
  const began = Date.now();
  await page.getByRole("button", { name: "Changes" }).click();
  await page.waitForFunction(() => document.querySelectorAll(".diff .file").length === 40);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  const openedMs = Date.now() - began;
  const gaps = await page.evaluate(
    (step) =>
      new Promise<number[]>((done) => {
        const scroller = document.querySelector(".diff")!;
        const gaps: number[] = [];
        let last = performance.now();
        const tick = (now: number) => {
          gaps.push(now - last);
          last = now;
          scroller.scrollTop += step;
          if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 1) {
            done(gaps);
          } else {
            requestAnimationFrame(tick);
          }
        };
        requestAnimationFrame(tick);
      }),
    STEP_PX,
  );
  const result = {
    diffLines: await page.evaluate(() => document.querySelectorAll(".diff .line").length),
    openedMs,
    ...(await pageMetrics(page, gaps)),
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} finally {
  await harness.close();
}
