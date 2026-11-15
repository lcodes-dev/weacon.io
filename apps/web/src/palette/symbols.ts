import type { Later, WebCapability } from "../connection.ts";
import type { MachineId, MachineSummary, Symbol as CodeSymbol, SymbolKind } from "../protocol.ts";
import { type Answer, MALFORMED } from "../api/api.ts";
import { type MachineSocket, askMachine } from "../machines/ask-machine.ts";
import { lacks } from "../machines/core.ts";

/** What a core offers to search its code. */
export const SYMBOLS: WebCapability = "symbols.search";

/** The machine the palette asks for symbols: the open chat's, by its name, when it offers them, else the first that does. */
export function searcher(machines: readonly MachineSummary[], chatMachine: string | undefined): MachineId | undefined {
  const offering = machines.filter(offers);
  return (offering.find((machine) => machine.name === chatMachine) ?? offering[0])?.id;
}

/**
 * Whether `machine` searches its code now: connected, with a core that said what it offers and
 * didn't leave out symbol search.
 */
const offers = (machine: MachineSummary) => machine.connected === true && machine.core !== undefined && !lacks(machine, SYMBOLS);

/** The items of the open workspace's code on `machine` that match `words`, best first. */
export function symbols(socket: MachineSocket, machine: MachineId, words: string, later: Later): Promise<Answer<CodeSymbol[]>> {
  return askMachine(
    socket,
    later,
    (id, request) => ({ id, type: "symbols_query", request, machine, workspace: socket.workspace, query: { kind: "search", words } }),
    (message, request) => {
      if (message.type !== "symbols_answer" || message.request !== request) {
        return undefined;
      }
      return message.answer.kind === "found" ? { ok: true, value: message.answer.symbols.filter(known) } : { ok: false, code: MALFORMED };
    },
  );
}

const KINDS: readonly SymbolKind[] = ["const", "enum", "fn", "macro", "mod", "static", "struct", "trait", "type", "union"];

// A symbol of a kind this build doesn't know is left out.
const known = (symbol: CodeSymbol) => KINDS.includes(symbol.kind);

/** Where `symbol` is, as the palette copies it. */
export const place = (symbol: CodeSymbol) => `${symbol.path}:${symbol.line}`;
