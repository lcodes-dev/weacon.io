import { useEffect, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { Names } from "../chat/label.ts";
import type { DocId, WorkspaceId } from "../protocol.ts";
import { useBurst } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { DocEditor } from "./doc-editor.tsx";
import { DocView } from "./doc-view.tsx";
import "./docs.css";

type Props = {
  api: Api;
  workspace: WorkspaceId;
  socket: WorkspaceSocket;
  names: Names;
  /** The doc open, or none for the list. */
  doc: DocId | undefined;
  choose(doc: DocId | undefined): void;
};

/** The workspace's docs by title, kept fresh by its feed, or the one `doc` names. */
export function DocsPage(props: Props) {
  const { doc, choose } = props;
  return doc === undefined ? <DocList {...props} /> : <DocView {...props} doc={doc} back={() => choose(undefined)} />;
}

function DocList({ api, workspace, socket, choose }: Props) {
  const docs = useLoaded(() => api.docs(workspace), workspace);
  const [making, setMaking] = useState(false);
  const creating = useAsk();
  const reload = useBurst(() => void docs.load());
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (message.type === "workspace" && message.change.kind === "doc_changed") {
          reload();
        }
      }),
    [socket],
  );
  const list = docs.value?.docs;
  if (making) {
    return (
      <div className="page docs">
        <DocEditor
          initial={{ title: "", body: "" }}
          busy={creating.busy}
          problem={creating.problem}
          save={(content) => void creating.ask(() => api.createDoc(workspace, content), (made) => choose(made.id))}
          cancel={() => {
            creating.clear();
            setMaking(false);
          }}
        />
      </div>
    );
  }
  return (
    <div className="page docs">
      {docs.loading && <p className="muted">Loading</p>}
      {list?.length === 0 && <p className="muted">No docs yet.</p>}
      {list !== undefined && list.length > 0 && (
        <ul aria-label="Docs">
          {list.map(({ id, title }) => (
            <li key={id}>
              <button type="button" onClick={() => choose(id)}>
                {title}
              </button>
            </li>
          ))}
        </ul>
      )}
      {docs.problem !== undefined && <Problem text={docs.problem} retry={() => void docs.load()} />}
      {list !== undefined && (
        <button type="button" onClick={() => setMaking(true)}>
          New doc
        </button>
      )}
    </div>
  );
}
