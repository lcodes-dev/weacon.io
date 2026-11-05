/** Whether `text` is a commit hash, by the rule `crates/version` applies too. */
export function isCommit(text: string): boolean {
  return /^[0-9a-f]{7,64}$/i.test(text);
}
