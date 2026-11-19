import { diffLines } from "diff"

/** The lines of a change from one text to another, each with its number in the old text, the new, or both. */
function changes(from: string, to: string) {
  let [old, now] = [0, 0]
  // A last line with and without its line end would count as changed.
  return diffLines(`${from.trimEnd()}\n`, `${to.trimEnd()}\n`).flatMap(({ value, added, removed }) =>
    value
      .replace(/\n$/, "")
      .split("\n")
      .map((text) => ({ text, added, removed, old: added ? undefined : ++old, now: removed ? undefined : ++now })),
  )
}

/** How many lines a change puts in and takes out. */
export function Counts({ from, to }: { from: string; to: string }) {
  const lines = changes(from, to)
  return (
    <span className="font-mono text-[11px]">
      <span className="text-ok">+{lines.filter((line) => line.added).length}</span>{" "}
      <span className="text-acc">−{lines.filter((line) => line.removed).length}</span>
    </span>
  )
}

/** What changed from one text to another, line by line. */
export function Diff({ from, to, title }: { from: string; to: string; title: string }) {
  const lines = changes(from, to)
  return (
    <div className="overflow-hidden rounded-lg border border-line font-mono text-[12px]">
      <div className="flex items-center justify-between gap-3 border-b border-line bg-sur px-3 py-1.5 font-sans text-ink2">
        {title}
        <Counts from={from} to={to} />
      </div>
      {lines.every((line) => !line.added && !line.removed) && <p className="px-3 py-2 font-sans text-ink2">Nothing changes in the text.</p>}
      {lines.map(({ text, added, removed, old, now }, place) => (
        // A diff is worked out anew each time, so a line has no name but its place.
        <div key={place} className={`flex ${added ? "bg-ok/15" : removed ? "bg-acc/15" : "text-ink2"}`}>
          <span className="w-9 shrink-0 pr-2 text-right text-ink3 select-none">{old}</span>
          <span className="w-9 shrink-0 pr-2 text-right text-ink3 select-none">{now}</span>
          <span className="w-4 shrink-0 select-none">{added ? "+" : removed ? "−" : ""}</span>
          <span className="min-w-0 flex-1 pr-3 break-words whitespace-pre-wrap">{text}</span>
        </div>
      ))}
    </div>
  )
}
