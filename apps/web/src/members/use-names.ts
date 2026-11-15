import { useEffect, useMemo } from "react";

import type { Api } from "../api/api.ts";
import { useLoaded } from "../api/loaded.ts";
import type { Names } from "../chat/label.ts";
import type { WorkspaceId } from "../protocol.ts";
import { useBurst } from "../shell/feed-hooks.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { namesOf } from "./names.ts";

/** The names of `workspace`'s members, where the server serves them, read again as members join. */
export function useNames(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket): Names {
  const served = useOffers(socket, "workspace.members");
  const members = useLoaded(() => (served ? api.members(workspace) : Promise.resolve({ ok: true as const, value: { members: [] } })), `${workspace} ${served}`);
  const reload = useBurst(() => void members.load());
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (served && message.type === "workspace" && message.change.kind === "member_joined") {
          reload();
        }
      }),
    [socket, served],
  );
  return useMemo(() => namesOf(members.value), [members.value]);
}
