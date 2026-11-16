import type { Later } from "../connection.ts";
import type { MachineId, MachineSummary, Symbol as CodeSymbol } from "../protocol.ts";
import type { Answer } from "../api/api.ts";
import { type MachineSocket, askMachine } from "../machines/ask-machine.ts";

/** The machine the palette asks for symbols: the open chat's when it is connected, else the first that is. */
export function searcher(machines: readonly MachineSummary[], chatMachine: MachineId | undefined): MachineId | undefined {
  const connected = machines.filter((machine) => machine.connected === true);
  return (connected.find((machine) => machine.id === chatMachine) ?? connected[0])?.id;
}

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
      return { ok: true, value: message.answer.symbols };
    },
  );
}

/** Where `symbol` is, as the palette copies it. */
export const place = (symbol: CodeSymbol) => `${symbol.path}:${symbol.line}`;
