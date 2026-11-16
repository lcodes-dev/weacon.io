import assert from "node:assert/strict";
import { test } from "node:test";

import { BUILD, core, pair } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

test("the machines list shows a core's build once it has connected", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const { token } = await pair(page);
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  // The list is read again only when the page is.
  const reopen = async () => {
    await page.reload();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await row.getByText("Linux, paired").waitFor();
  };
  await reopen();
  assert.equal(await row.getByText(`weacon ${BUILD.version}`).count(), 0);

  const machine = await core(page, token, { access: "ask_first", sign_ins: [] });
  t.after(() => machine.close());
  await reopen();
  await row.getByText(`weacon ${BUILD.version}`).waitFor();
});
