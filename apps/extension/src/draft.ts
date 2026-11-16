/** The composer's text once `sent` has gone: empty, unless the person typed on meanwhile. */
export function afterSend(now: string, sent: string): string {
  return now === sent ? "" : now;
}
