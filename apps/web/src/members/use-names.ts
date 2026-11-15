import { useMemo } from "react";

import type { Api } from "../api/api.ts";
import { type Loading, answered, useLoaded } from "../api/loaded.ts";
import type { MemberList, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { type Names, namesOf } from "./names.ts";

/** `workspace`'s members, where the server serves them, read again as they join, leave or change. */
export function useMembers(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket): Loading<MemberList> {
  const served = useOffers(socket, "workspace.members");
  const members = useLoaded(() => (served ? api.members(workspace) : answered({ members: [] })), `${workspace} ${served}`);
  useReloadOn(socket, ({ kind }) => served && (kind === "member_joined" || kind === "members_changed"), () => void members.load(), [served]);
  return members;
}

/** The names of `members`, by id. */
export function useNames(members: Loading<MemberList>): Names {
  return useMemo(() => namesOf(members.value), [members.value]);
}
