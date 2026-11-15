import { useMemo } from "react";

import type { Api } from "../api/api.ts";
import { answered, useLoaded } from "../api/loaded.ts";
import type { WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { type Names, namesOf } from "./names.ts";

/** The names of `workspace`'s members, where the server serves them, read again as members join. */
export function useNames(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket): Names {
  const served = useOffers(socket, "workspace.members");
  const members = useLoaded(() => (served ? api.members(workspace) : answered({ members: [] })), `${workspace} ${served}`);
  useReloadOn(socket, ({ kind }) => served && kind === "member_joined", () => void members.load(), [served]);
  return useMemo(() => namesOf(members.value), [members.value]);
}
