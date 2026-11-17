import { useMemo } from "react";

import type { Api } from "../api/api.ts";
import { type Loading, useLoaded } from "../api/loaded.ts";
import type { MemberList, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { type Names, namesOf } from "./names.ts";

/** `workspace`'s members, read again as they join, leave or change. */
export function useMembers(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket): Loading<MemberList> {
  const members = useLoaded(() => api.members(workspace), workspace);
  useReloadOn(socket, ({ kind }) => kind === "member_joined" || kind === "members_changed", () => void members.load());
  return members;
}

/** The names of `members`, by id. */
export function useNames(members: Loading<MemberList>): Names {
  return useMemo(() => namesOf(members.value), [members.value]);
}
