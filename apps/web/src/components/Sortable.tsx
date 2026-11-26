import { createChange, createReorder, definePrimitive, type ChangeDetails, type ReorderDraft, type ReorderReason } from "@ui-sdk/core"
import { createParts } from "@ui-sdk/react/primitive"

type Options = {
  value?: string[]
  onValueChange?: (value: string[], details: ChangeDetails<ReorderReason>) => void
}

/** A list whose items are put in order by dragging a handle, or with the keyboard on it. */
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
