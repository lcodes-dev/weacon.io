import { createChange, createTransfer, definePrimitive, type ChangeDetails, type TransferDraft, type TransferLists, type TransferReason } from "@ui-sdk/core"
import { createParts } from "@ui-sdk/react/primitive"

/** `to` is the column the card was dropped in. */
type Moved = ChangeDetails<TransferReason> & { to: string }

type Options = {
  /** Each column's cards, in order, by the column's key. */
  value?: TransferLists
  onValueChange?: (value: TransferLists, details: Moved) => void
}

/** Columns of cards, where a card is dragged to another place or column, or moved with the keyboard on it. */
const board = definePrimitive({
  name: "board",
  options: { value: "value", onValueChange: "callback" },
  controlled: ["value"],
  state: (options: Options) => ({ value: options.value ?? {}, moving: null as TransferDraft | null }),
  sync: (options) => (options.value === undefined ? {} : { value: options.value }),
  api: (context) => {
    const change = createChange(context)
    return {
      transfer: createTransfer(context, {
        key: "value",
        draft: "moving",
        set: (value, details) => change("value", value, details),
      }),
    }
  },
  slots: (slot) => ({
    root: slot<{}, { lists: TransferLists }>({
      state: (state, part) => ({ lists: part.api.transfer.lists(state) }),
    }),
    column: slot<{ value: string }>({
      role: "list",
      props: { value: "string" },
      bind: (el, part) => part.api.transfer.bindList(el, part.key),
    }),
    card: slot<{ value: string }>({
      role: "listitem",
      props: { value: "string" },
      attrs: (state, part) => ({ "data-grabbed": part.api.transfer.moving(state, part.key) ? "" : undefined }),
      on: {
        // A key pressed on a link inside the card is the link's.
        keydown: (event, { el, api, key }) => void (event.target === el && api.transfer.keydown(event, key)),
        pointerdown: (event, { api, key }) => api.transfer.pointerdown(event, key),
      },
      bind: (el, part) => part.api.transfer.bindItem(el, part.key),
    }),
  }),
})

export const { Root: Board, Column: BoardColumn, Card: BoardCard } = createParts(board, { root: "div", column: "div", card: "div" })
