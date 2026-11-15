import { useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { Names } from "../members/names.ts";
import type { DocContent, DocId, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
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
  return doc === undefined ? <DocList {...props} /> : <DocView key={doc} {...props} doc={doc} back={() => choose(undefined)} />;
}

function DocList({ api, workspace, socket, choose }: Props) {
  const docs = useLoaded(() => api.docs(workspace), workspace);
  const [making, setMaking] = useState<DocContent>();
  const creating = useAsk();
  useReloadOn(socket, ({ kind }) => kind === "doc_changed", () => void docs.load());
  const list = docs.value?.docs;
  if (making !== undefined) {
    return (
      <div className="page docs">
        <DocEditor
          content={making}
          change={setMaking}
          busy={creating.busy}
          problem={creating.problem}
          save={(content) => void creating.ask(() => api.createDoc(workspace, content), (made) => choose(made.id))}
          cancel={() => {
            creating.clear();
            setMaking(undefined);
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
        <button type="button" onClick={() => setMaking({ title: "", body: "" })}>
          New doc
        </button>
      )}
    </div>
  );
}
