import { type Names, authorText } from "../members/names.ts";
import type { DocRevision } from "../protocol.ts";

/** Who made a doc's `revision`, and who accepted it when that was someone else, as its page says. */
export function byline(revision: DocRevision | undefined, names: Names): string | undefined {
  if (revision === undefined) {
    return undefined;
  }
  const { author, accepted_by } = revision;
  const by = `Revision ${revision.version} by ${authorText(author, names)}`;
  return author.kind === "person" && author.member === accepted_by ? by : `${by}, accepted by ${names.get(accepted_by) ?? "a member"}`;
}
