import type { WorkspaceRole } from "../protocol.ts";

/** A role as a person reads it; one this build doesn't know reads as a member's. */
export function roleText(role: WorkspaceRole): string {
  return role === "owner" ? "Owner" : "Member";
}
