import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../src/protocol.ts";
import { FAKE_CHAT, fakeServer, start, waitFor } from "./harness.ts";

// The page the fake core opens a pull request on.
const COMPARE = "https://github.com/example/shopping-list/compare/weacon-c-1?expand=1";

test("the review bar says where the worktree stands, and Remove asks first, then Restore brings it back", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), { query: FAKE_CHAT });
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });

  await bar.getByText("Uncommitted changes · Commits not pushed").waitFor();
  await bar.getByText("weacon resume --workspace=w-1 --chat=c-1").waitFor();
  // This core opens no terminals.
  await bar.getByRole("button", { name: "Open terminal" }).click();
  await bar.getByText("The terminal didn't open.").waitFor();
  await bar.getByRole("button", { name: "Remove" }).click();
  const confirm = bar.getByRole("group", { name: "Remove the worktree" });
  await confirm
    .getByText("Remove this chat's worktree? Its uncommitted changes go with it to the Trash. Its branch stays, with commits that aren't pushed yet. You can restore it from here.")
    .waitFor();
  await confirm.getByRole("button", { name: "Remove" }).click();
  await bar.getByText("The worktree is in the Trash.").waitFor();
  await bar.getByRole("button", { name: "Restore" }).click();
  await bar.getByText("Uncommitted changes · Commits not pushed").waitFor();

  const actions = (harness.received() as Envelope[]).flatMap((sent) => (sent.type === "worktree_command" ? [sent.action.kind] : []));
  assert.deepEqual(actions, ["open_terminal", "remove", "restore"]);
});

test("Cancel leaves the worktree where it is", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), { query: FAKE_CHAT });
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });
  await bar.getByRole("button", { name: "Remove" }).click();
  await bar.getByRole("group", { name: "Remove the worktree" }).getByRole("button", { name: "Cancel" }).click();

  await bar.getByRole("button", { name: "Open pull request" }).waitFor();
  const sent = harness.received() as Envelope[];
  assert.equal(sent.filter((envelope) => envelope.type === "worktree_command").length, 0);
  // Remove asked for a fresh state before it asked the person.
  assert.equal(sent.filter((envelope) => envelope.type === "worktree_query").length, 2);
});

test("Open pull request sends the title and description, and links to the page the core gives", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), { query: FAKE_CHAT });
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });
  await bar.getByRole("button", { name: "Open pull request" }).click();

  const open = bar.getByRole("button", { name: "Open draft pull request" });
  assert.equal(await bar.getByLabel("Description").inputValue(), "");
  await bar.getByLabel("Title").fill("Sort the pantry");
  await open.click();
  const link = bar.getByRole("link", { name: "Open it" });
  await link.waitFor();

  assert.equal(await link.getAttribute("href"), COMPARE);
  await waitFor(() => (harness.received() as Envelope[]).some((sent) => sent.type === "worktree_command"));
  const actions = (harness.received() as Envelope[]).flatMap((sent) => (sent.type === "worktree_command" ? [sent.action] : []));
  assert.deepEqual(actions, [{ kind: "open_pull_request", title: "Sort the pantry", body: "" }]);
});
