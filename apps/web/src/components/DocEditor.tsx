import { Editor, defaultValueCtx, editorViewCtx, remarkStringifyOptionsCtx, rootCtx } from "@milkdown/kit/core"
import { clipboard } from "@milkdown/kit/plugin/clipboard"
import { history, redoCommand, undoCommand } from "@milkdown/kit/plugin/history"
import { upload, uploadConfig } from "@milkdown/kit/plugin/upload"
import {
  commonmark,
  createCodeBlockCommand,
  insertHrCommand,
  toggleEmphasisCommand,
  toggleInlineCodeCommand,
  toggleLinkCommand,
  toggleStrongCommand,
  turnIntoTextCommand,
  wrapInBlockquoteCommand,
  wrapInBulletListCommand,
  wrapInHeadingCommand,
  wrapInOrderedListCommand,
} from "@milkdown/kit/preset/commonmark"
import { gfm, insertTableCommand, toggleStrikethroughCommand } from "@milkdown/kit/preset/gfm"
import type { Schema } from "@milkdown/kit/prose/model"
import type { EditorView } from "@milkdown/kit/prose/view"
import "@milkdown/kit/prose/view/style/prosemirror.css"
import { callCommand, getMarkdown } from "@milkdown/kit/utils"
import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Toolbar, ToolbarButton, ToolbarSeparator } from "@ui-sdk/react/toolbar"
import {
  TriggerMenu,
  TriggerMenuContent,
  TriggerMenuEmpty,
  TriggerMenuItem,
  TriggerMenuItemIcon,
  TriggerMenuList,
  type EditorChange,
  type TextEditorAdapter,
} from "@ui-sdk/react/trigger-menu"
import {
  BoldIcon,
  CodeIcon,
  Heading2Icon,
  Heading3Icon,
  ImageIcon,
  ItalicIcon,
  LinkIcon,
  ListIcon,
  ListOrderedIcon,
  MinusIcon,
  PaperclipIcon,
  QuoteIcon,
  RedoIcon,
  SquareCodeIcon,
  StrikethroughIcon,
  TableIcon,
  UndoIcon,
  type LucideIcon,
} from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { api, type DocsPage } from "../api"
import { Diff } from "./Diff"

// A command has its key only once an editor is made, so the key is read when the command is called.
type Command = { key: Parameters<typeof callCommand>[0] }

/** What the toolbar offers, in its order. Those with `block` are also slash commands. */
const tools: { name: string; icon: LucideIcon; group: number; block?: true; commands?: [Command, unknown?][] }[] = [
  { name: "Bold", icon: BoldIcon, group: 0, commands: [[toggleStrongCommand]] },
  { name: "Italic", icon: ItalicIcon, group: 0, commands: [[toggleEmphasisCommand]] },
  { name: "Strikethrough", icon: StrikethroughIcon, group: 0, commands: [[toggleStrikethroughCommand]] },
  { name: "Inline code", icon: CodeIcon, group: 0, commands: [[toggleInlineCodeCommand]] },
  { name: "Link", icon: LinkIcon, group: 0 },
  { name: "Heading", icon: Heading2Icon, group: 1, block: true, commands: [[wrapInHeadingCommand, 2]] },
  { name: "Small heading", icon: Heading3Icon, group: 1, block: true, commands: [[wrapInHeadingCommand, 3]] },
  // A list only wraps plain text, so a heading is turned into text first.
  { name: "Bulleted list", icon: ListIcon, group: 1, block: true, commands: [[turnIntoTextCommand], [wrapInBulletListCommand]] },
  { name: "Numbered list", icon: ListOrderedIcon, group: 1, block: true, commands: [[turnIntoTextCommand], [wrapInOrderedListCommand]] },
  { name: "Quote", icon: QuoteIcon, group: 1, block: true, commands: [[wrapInBlockquoteCommand]] },
  { name: "Code block", icon: SquareCodeIcon, group: 1, block: true, commands: [[createCodeBlockCommand]] },
  { name: "Table", icon: TableIcon, group: 1, block: true, commands: [[insertTableCommand]] },
  { name: "Divider", icon: MinusIcon, group: 1, block: true, commands: [[insertHrCommand]] },
  { name: "Image", icon: ImageIcon, group: 1, block: true },
  { name: "Attachment", icon: PaperclipIcon, group: 1, block: true },
  { name: "Undo", icon: UndoIcon, group: 2, commands: [[undoCommand]] },
  { name: "Redo", icon: RedoIcon, group: 2, commands: [[redoCommand]] },
]

const images = "image/png,image/jpeg,image/gif,image/webp"

const size = (bytes: number) => (bytes < 1e6 ? `${Math.ceil(bytes / 1e3)} kB` : `${(bytes / 1e6).toFixed(1)} MB`)

/** Lets UI SDK's trigger menu read and write the editor. An offset is a position in the document. */
function adapter(view: EditorView): TextEditorAdapter {
  const listeners = new Set<(change: EditorChange) => void>()
  // Every change to the editor goes through its dispatch, so that is where to listen.
  const prior = view.props.dispatchTransaction
  view.setProps({
    dispatchTransaction(tr) {
      if (prior) prior.call(view, tr)
      else view.updateState(view.state.apply(tr))
      const tell = (change: EditorChange) => listeners.forEach((listener) => listener(change))
      if (!tr.docChanged) return tr.selectionSet ? tell({ type: "selection" }) : undefined
      for (const step of tr.steps) {
        step.getMap().forEach((start, end, newStart, newEnd) => {
          const inserted = tr.doc.textBetween(newStart, Math.min(newEnd, tr.doc.content.size), undefined, "￼")
          tell({ type: "input", inserted, range: { start, end }, user: true })
        })
      }
    },
  })
  return {
    element: view.dom,
    textBefore: (length) => {
      const { $from } = view.state.selection
      if ($from.parent.type.spec.code) return null
      const offset = Math.max($from.start(), $from.pos - length)
      const text = view.state.doc.textBetween(offset, $from.pos, undefined, "￼")
      // A block's start reads as a line's start, so that "/" opens the menu there.
      return offset === $from.start() ? { text: `\n${text}`, offset: offset - 1 } : { text, offset }
    },
    selection: () => (view.hasFocus() ? { start: view.state.selection.from, end: view.state.selection.to } : null),
    rectAt: (offset) => {
      const at = view.coordsAtPos(Math.min(offset, view.state.doc.content.size))
      return { x: at.left, y: at.top, width: 0, height: at.bottom - at.top }
    },
    replace: ({ start, end }, content) => {
      if (typeof content === "string") view.dispatch(content ? view.state.tr.insertText(content, start, end) : view.state.tr.delete(start, end))
    },
    subscribe: (listener) => {
      const stop = new AbortController()
      view.dom.addEventListener("compositionstart", () => listener({ type: "compositionstart" }), { signal: stop.signal })
      view.dom.addEventListener("compositionend", () => listener({ type: "compositionend" }), { signal: stop.signal })
      listeners.add(listener)
      return () => {
        stop.abort()
        listeners.delete(listener)
      }
    },
    composing: () => view.composing,
  }
}

/** A page edited as it reads, and saved as Markdown after its changes are shown. `path` is the project's API path. */
export function DocEditor({ path, page, onDone }: { path: string; page: DocsPage; onDone: () => void }) {
  const queryClient = useQueryClient()
  const root = useRef<HTMLDivElement>(null)
  const file = useRef<HTMLInputElement>(null)
  // The Markdown the editor writes for the page untouched, which tells whether anything was typed.
  const untouched = useRef("")
  const [editor, setEditor] = useState<Editor>()
  const [title, setTitle] = useState(page.title)
  // The Markdown about to be saved, while its changes are shown.
  const [written, setWritten] = useState<string>()
  const [failed, setFailed] = useState<Error>()
  // The version this edit started from. The page moving past it means someone else saved meanwhile.
  const [base] = useState(page.version)
  const moved = page.version !== base
  const refetch = () => queryClient.invalidateQueries({ queryKey: ["docs"] })
  const save = useMutation({
    mutationFn: () => api("PUT", `${path}/docs/pages/${page.id}`, { title, markdown: written, version: page.version }),
    onSuccess: () => refetch().then(onDone),
    onError: refetch,
  })
  // In a page an image is `image:{id}`; in the editor it is the address the browser fetches.
  const files = `/api${path}/files/`
  /** Keeps a file for the project and answers with what to put in the page: the image, or a link to the attachment. */
  const keep = async (file: File, schema: Schema) => {
    try {
      const kept = await api<{ id: string; image: boolean }>("POST", `${path}/files?name=${encodeURIComponent(file.name)}`, file)
      if (kept.image) return schema.nodes.image.create({ src: files + kept.id })
      return schema.text(`${file.name} (${size(file.size)})`, [schema.marks.link.create({ href: `file:${kept.id}` })])
    } catch (error) {
      setFailed(error as Error)
    }
  }
  // The editor writes an empty paragraph as a line break in HTML, which a page would show as text.
  const markdown = (editor: Editor) =>
    editor
      .action(getMarkdown())
      .replace(/^<br \/>\n+/gm, "")
      .replaceAll(`(${files}`, "(image:")
      .replace(/(!\[[^\]]*\]\()unloaded:/g, "$1")
      .trimEnd()
  // An image on another host would be fetched on sight, telling it who edits what, so its
  // address is made one a browser doesn't fetch until the page is saved.
  const loaded = page.markdown.replaceAll("(image:", `(${files}`).replace(/(!\[[^\]]*\]\()(?=https?:)/g, "$1unloaded:")

  useEffect(() => {
    let gone = false
    const made = Editor.make()
      .config((ctx) => {
        ctx.set(rootCtx, root.current)
        ctx.set(defaultValueCtx, loaded)
        // Write lists and dividers the way the docs writer does, so that a save changes less.
        ctx.update(remarkStringifyOptionsCtx, (options) => ({ ...options, bullet: "-" as const, rule: "-" as const }))
        ctx.update(uploadConfig.key, (config) => ({
          ...config,
          uploader: async (files, schema) => {
            const kept = await Promise.all([...files].map((file) => keep(file, schema)))
            return kept.filter((node) => node !== undefined)
          },
        }))
      })
      .use(commonmark)
      .use(gfm)
      .use(history)
      .use(clipboard)
      .use(upload)
      .create()
    made.then((editor) => {
      if (gone) return
      untouched.current = markdown(editor)
      setEditor(editor)
    })
    return () => {
      gone = true
      made.then((editor) => editor.destroy())
    }
  }, [])

  const view = useMemo(() => editor?.ctx.get(editorViewCtx), [editor])
  const menu = useMemo(() => view && adapter(view), [view])
  const call = (command: Command, payload?: unknown) => editor?.action(callCommand(command.key, payload))
  const run = (name: string) => {
    if (file.current && (name === "Image" || name === "Attachment")) {
      file.current.accept = name === "Image" ? images : ""
      return file.current.click()
    }
    if (name === "Link") {
      const href = prompt("Link to")
      if (href) call(toggleLinkCommand, { href })
    }
    tools.find((tool) => tool.name === name)?.commands?.forEach((command) => call(...command))
    view?.focus()
  }
  const review = () => {
    const now = markdown(editor!)
    if (now === untouched.current && title === page.title) return onDone()
    setFailed(undefined)
    // With only the title changed, the text is saved as it was and not as the editor would write it.
    setWritten(now === untouched.current ? page.markdown : now)
  }
  const error = failed ?? (moved ? null : save.error)
  return (
    <div className="flex max-w-[720px] min-w-0 flex-1 flex-col gap-3">
      {written !== undefined && (
        <>
          <h2 className="font-semibold">What saving changes</h2>
          {moved && <p className="text-acc">Someone else changed this page while you were editing. This is what saving yours would change in theirs.</p>}
          {title !== page.title && (
            <p>
              The title "{page.title}" becomes "{title}".
            </p>
          )}
          <Diff title={page.title} from={page.markdown} to={written} />
          <div className="flex gap-2">
            <button className="btn btn-primary" disabled={save.isPending} onClick={() => save.mutate()}>
              {moved ? "Save mine over theirs" : "Save"}
            </button>
            <button className="btn" onClick={() => setWritten(undefined)}>
              Keep editing
            </button>
          </div>
        </>
      )}
      <div className="editor" hidden={written !== undefined}>
        <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-line bg-bg py-1.5">
          <Toolbar aria-label="Formatting" className="mr-auto">
            {tools.map(({ name, icon: Icon, group }, place) => (
              <span key={name} className="contents">
                {place > 0 && tools[place - 1].group !== group && <ToolbarSeparator />}
                <ToolbarButton aria-label={name} title={name} onMouseDown={(event) => event.preventDefault()} onClick={() => run(name)}>
                  <Icon aria-hidden="true" />
                </ToolbarButton>
              </span>
            ))}
          </Toolbar>
          <button className="btn" onClick={onDone}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={!editor || !title.trim()} onClick={review}>
            Save
          </button>
        </div>
        <input
          ref={file}
          type="file"
          className="sr-only"
          aria-label="File to add"
          onChange={async (event) => {
            const picked = event.target.files?.[0]
            event.target.value = ""
            const node = picked && view && (await keep(picked, view.state.schema))
            if (node) view.dispatch(view.state.tr.replaceSelectionWith(node, false))
          }}
        />
        <TriggerMenu
          editor={menu}
          triggers={[{ char: "/" }]}
          onSelect={(name, details) => {
            details.insert("")
            run(name)
          }}
        >
          <TriggerMenuContent>
            <TriggerMenuEmpty>No such command</TriggerMenuEmpty>
            <TriggerMenuList>
              {tools
                .filter((tool) => tool.block)
                .map(({ name, icon: Icon }) => (
                  <TriggerMenuItem key={name} value={name} label={name}>
                    <TriggerMenuItemIcon>
                      <Icon />
                    </TriggerMenuItemIcon>
                    {name}
                  </TriggerMenuItem>
                ))}
            </TriggerMenuList>
          </TriggerMenuContent>
        </TriggerMenu>
        <div className="prose pt-4">
          <input aria-label="Title" className="text-[20px] font-semibold outline-none" value={title} onChange={(event) => setTitle(event.target.value)} />
          <div ref={root} />
        </div>
      </div>
      {error && <p className="text-acc">{error.message}</p>}
    </div>
  )
}
