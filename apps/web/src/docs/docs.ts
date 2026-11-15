import { type Names, authorText } from "../members/names.ts";
import type { DocRecord, DocRevision } from "../protocol.ts";

/** Who made a doc's `revision`, and who accepted it when that was someone else, as its page says. */
export function byline(revision: DocRevision | undefined, names: Names): string | undefined {
  if (revision === undefined) {
    return undefined;
  }
  const { author, accepted_by } = revision;
  const by = `Revision ${revision.version} by ${authorText(author, names)}`;
  return author.kind === "person" && author.member === accepted_by ? by : `${by}, accepted by ${names.get(accepted_by) ?? "a member"}`;
}

/** The newer of the doc `shown` and the one a save answered with, so a save never takes back a later revision. */
export function newer(shown: DocRecord | undefined, saved: DocRecord): DocRecord {
  return shown !== undefined && shown.id === saved.id && shown.version > saved.version ? shown : saved;
}
