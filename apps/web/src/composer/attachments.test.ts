import assert from "node:assert/strict";
import { test } from "node:test";

import { MAX_BYTES, MAX_FILES, NONE, add, remove, settled, size } from "./attachments.ts";

const file = (name: string, bytes: number) => new File([new Uint8Array(bytes)], name);

test("files are added in order, each with an id of its own", () => {
  const first = add(NONE, [file("a.txt", 3), file("b.png", 5)]);
  const second = add(first, [file("c.md", 1)]);

  assert.deepEqual(
    second.attachments.map(({ id, file: { name, size: bytes } }) => [id, name, bytes]),
    [
      [1, "a.txt", 3],
      [2, "b.png", 5],
      [3, "c.md", 1],
    ],
  );
  assert.equal(second.refused, undefined);
  assert.equal(second.next, 4);
});

test("removing a file keeps the others and their ids, and clears why any were left out", () => {
  const added = add(NONE, [file("a", 1), file("b", 1), file("c", 0)]);
  const left = remove(added, 1);
  assert.deepEqual(
    left.attachments.map(({ id, file }) => [id, file.name]),
    [[2, "b"]],
  );
  assert.equal(left.refused, undefined);
  assert.equal(add(left, [file("d", 1)]).attachments.at(-1)?.id, 3);
});

test("empty and oversized files are left out, and the box says why", () => {
  const added = add(NONE, [file("folder", 0), file("big.bin", MAX_BYTES + 1), file("ok.bin", MAX_BYTES)]);
  assert.deepEqual(
    added.attachments.map(({ file }) => file.name),
    ["ok.bin"],
  );
  assert.equal(added.refused, "Files must be 10 MB or smaller, and not empty");
  assert.equal(added.next, 2);
});

test("past the limit, the extra files are left out", () => {
  const full = add(NONE, Array.from({ length: MAX_FILES - 1 }, (_, at) => file(`${at}`, 1)));
  const over = add(full, [file("last", 1), file("one too many", 1)]);

  assert.equal(over.attachments.length, MAX_FILES);
  assert.equal(over.attachments.at(-1)?.file.name, "last");
  assert.equal(over.refused, "A message can carry 10 files");
  assert.equal(add(over, [file("more", 1)]).attachments.length, MAX_FILES);
});

test("sizes read in B, KB or MB", () => {
  assert.equal(size(0), "0 B");
  assert.equal(size(1023), "1023 B");
  assert.equal(size(1024), "1 KB");
  assert.equal(size(1536), "2 KB");
  assert.equal(size(1024 * 1024 - 1), "1024 KB");
  assert.equal(size(1024 * 1024), "1 MB");
  assert.equal(size(2.25 * 1024 * 1024), "2.3 MB");
});

test("once the person moves on, why files were left out no longer shows", () => {
  const refused = add(NONE, [file("a", 1), file("empty", 0)]);
  assert.deepEqual(settled(refused), { ...refused, refused: undefined });
  const calm = add(NONE, [file("a", 1)]);
  assert.equal(settled(calm), calm);
});
