import Markdown from "markdown-to-jsx"
import { GripVerticalIcon } from "lucide-react"
import { type ComponentType, Fragment, type ReactNode } from "react"
import { type ChangeDetails, createChange, createReorder, definePrimitive, type ReorderDraft, type ReorderReason } from "@ui-sdk/core"
import { createParts } from "@ui-sdk/react/primitive"

export type DocPage = { id: string; title: string; markdown: string }

type Rendering = {
  code?: ComponentType<{ children?: ReactNode }>
  file: (id: string) => string
}

export function safeUrl(value: string) {
  try {
    const { protocol } = new URL(value, "https://weacon.invalid")
    return /^(https?|ircs?|mailto|xmpp|image|file):$/.test(protocol) ? value : null
  } catch {
    return null
  }
}

export function DocArticle({ title, markdown, code, file }: { title: string; markdown: string } & Rendering) {
  const img = ({ src, alt }: { src?: string; alt?: string }) =>
    src?.startsWith("image:") ? <img src={file(src.slice(6))} alt={alt ?? ""} /> : <a href={src}>{alt || src}</a>
  const a = ({ href, children }: { href?: string; children?: ReactNode }) =>
    href?.startsWith("file:") ? (
      <a href={file(href.slice(5))} download>
        {children}
      </a>
    ) : (
      <a href={href}>{children}</a>
    )
  return (
    <article className="prose min-w-0 flex-1">
      <h1>{title}</h1>
      <Markdown
        options={{
          disableParsingRawHTML: true,
          forceBlock: true,
          wrapper: Fragment,
          sanitizer: safeUrl,
          overrides: code ? { code, img, a } : { img, a },
        }}
      >
        {markdown}
      </Markdown>
    </article>
  )
}

export function DocPages<Page extends DocPage>(
  props: {
    pages: Page[]
    open: string | null
    onOpen: (id: string) => void
    onOrder?: (ids: string[]) => void
    note?: ReactNode
    children?: (page: Page) => ReactNode
  } & Rendering,
) {
  const page = props.pages.find((page) => page.id === props.open) ?? props.pages[0]
  const link = (entry: Page) => (
    <button
      aria-current={entry === page ? "page" : undefined}
      className="min-w-0 flex-1 rounded-md px-2 py-1 text-left phone:min-h-11 hover:bg-hov aria-[current]:bg-hov aria-[current]:font-medium"
      onClick={() => props.onOpen(entry.id)}
    >
      {entry.title}
    </button>
  )
  return (
    <div className="flex items-start gap-8 pt-4 phone:flex-col phone:items-stretch phone:gap-4">
      <nav aria-label="Pages" className="flex w-[220px] shrink-0 flex-col phone:w-full">
        {props.onOrder ? (
          <Sortable value={props.pages.map((page) => page.id)} onValueChange={props.onOrder} className="flex flex-col">
            {({ order }) =>
              order.flatMap((id) => {
                const entry = props.pages.find((page) => page.id === id)
                if (!entry) return []
                return (
                  <SortableItem key={id} value={id} className="group flex items-center">
                    <SortableHandle value={id} aria-label={`Move ${entry.title}`} className="icb w-5 cursor-grab opacity-0 group-hover:opacity-100 focus-visible:opacity-100 phone:hidden">
                      <GripVerticalIcon aria-hidden="true" />
                    </SortableHandle>
                    {link(entry)}
                  </SortableItem>
                )
              })
            }
          </Sortable>
        ) : (
          props.pages.map((entry) => <Fragment key={entry.id}>{link(entry)}</Fragment>)
        )}
        {props.note}
      </nav>
      {page && (props.children?.(page) ?? <DocArticle {...page} code={props.code} file={props.file} />)}
    </div>
  )
}

type Options = {
  value?: string[]
  onValueChange?: (value: string[], details: ChangeDetails<ReorderReason>) => void
}

const sortable = definePrimitive({
  name: "sortable",
  options: { value: "list", onValueChange: "callback" },
  controlled: ["value"],
  state: (options: Options) => ({ value: options.value ?? [], moving: null as ReorderDraft | null }),
  sync: (options) => (options.value === undefined ? {} : { value: options.value }),
  api: (context) => {
    const change = createChange(context)
    return {
      reorder: createReorder(context, {
        slot: "item",
        key: "value",
        draft: "moving",
        set: (value, details) => change("value", value, details),
      }),
    }
  },
  slots: (slot) => ({
    root: slot<{}, { order: string[] }>({
      role: "list",
      state: (state, part) => ({ order: part.api.reorder.order(state) }),
    }),
    item: slot<{ value: string }>({
      role: "listitem",
      props: { value: "string" },
      attrs: (state, part) => ({ "data-grabbed": part.api.reorder.moving(state, part.key) ? "" : undefined }),
      bind: (el, part) => part.api.reorder.bind(el, part),
    }),
    handle: slot<{ value: string }>({
      props: { value: "string" },
      attrs: () => ({ type: "button" }),
      on: {
        keydown: (event, { api, key }) => void api.reorder.keydown(event, key),
        pointerdown: (event, { api, key }) => api.reorder.pointerdown(event, key),
      },
    }),
  }),
})

export const { Root: Sortable, Item: SortableItem, Handle: SortableHandle } = createParts(sortable, { root: "div", item: "div", handle: "button" })
