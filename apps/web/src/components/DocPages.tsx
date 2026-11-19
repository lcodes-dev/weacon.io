import Markdown from "markdown-to-jsx"
import { Fragment, type ComponentType, type ReactNode } from "react"

export type DocPage = { id: string; title: string; markdown: string }

function safeUrl(value: string) {
  try {
    const { protocol } = new URL(value, "https://weacon.invalid")
    return /^(https?|ircs?|mailto|xmpp):$/.test(protocol) ? value : null
  } catch {
    return null
  }
}

// An image would be fetched from its host on sight, telling it who reads what; a link waits for a click.
const img = ({ src, alt }: { src?: string; alt?: string }) => <a href={src}>{alt || src}</a>

/** A list of docs pages beside the open one. `code` renders inline code, and `note` sits under the list. */
export function DocPages(props: {
  pages: DocPage[]
  open: string | null
  onOpen: (id: string) => void
  code?: ComponentType<{ children?: ReactNode }>
  note?: ReactNode
}) {
  const page = props.pages.find((page) => page.id === props.open) ?? props.pages[0]
  if (!page) return null
  return (
    <div className="flex items-start gap-8 pt-2">
      <nav aria-label="Pages" className="flex w-[200px] shrink-0 flex-col">
        {props.pages.map((entry) => (
          <button
            key={entry.id}
            aria-current={entry === page ? "page" : undefined}
            className="rounded-md px-2 py-1 text-left hover:bg-hov aria-[current]:bg-hov aria-[current]:font-medium"
            onClick={() => props.onOpen(entry.id)}
          >
            {entry.title}
          </button>
        ))}
        {props.note}
      </nav>
      <article className="prose min-w-0 flex-1">
        <h1>{page.title}</h1>
        <Markdown
          options={{
            disableParsingRawHTML: true,
            forceBlock: true,
            wrapper: Fragment,
            sanitizer: safeUrl,
            overrides: props.code ? { code: props.code, img } : { img },
          }}
        >
          {page.markdown}
        </Markdown>
      </article>
    </div>
  )
}
