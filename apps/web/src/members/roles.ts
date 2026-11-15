import type { WorkspaceRole } from "../protocol.ts";

const ROLES = new Map<WorkspaceRole | string, string>([
  ["owner", "Owner"],
  ["admin", "Admin"],
  ["member", "Member"],
]);

/** A role as a person reads it; one this build doesn't know reads as a member's. */
export function roleText(role: WorkspaceRole): string {
  return ROLES.get(role) ?? "Member";
}

/** Whether `role` may invite people and remove members. */
export function manages(role: WorkspaceRole | undefined): boolean {
  return role === "owner" || role === "admin";
}

/** Whether someone who is `by` may remove, or change the role of, someone who is `role`. */
export function mayManage(by: WorkspaceRole | undefined, role: WorkspaceRole | undefined): boolean {
  return (by === "owner" && (role === "admin" || role === "member")) || (by === "admin" && role === "member");
}
