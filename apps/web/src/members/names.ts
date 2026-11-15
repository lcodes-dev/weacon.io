import { label } from "../chat/label.ts";
import type { Author, MemberId, MemberList } from "../protocol.ts";

/** Each member's name, by their id. */
export type Names = ReadonlyMap<MemberId, string>;

/** The names `list` gives its members, none before it is loaded. */
export function namesOf(list: MemberList | undefined): Names {
  return new Map(list?.members.map(({ id, name }) => [id, name]));
}

/** Who made a change, as a person reads it; a member whose name isn't known is "A member". */
export function authorText(author: Author, names: Names): string {
  switch (author.kind) {
    case "person":
      return names.get(author.member) ?? "A member";
    case "agent":
      return label(author.agent, names, author.owner);
    default:
      return "Someone";
  }
}
