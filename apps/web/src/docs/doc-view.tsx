import { useEffect, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { Names } from "../chat/label.ts";
import type { DocContent, DocId, DocRecord, WorkspaceId } from "../protocol.ts";
import { DiffLines } from "../proposals/diff-lines.tsx";
import { useBurst } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { MarkdownView } from "../transcript/markdown-view.ts";
import { DocEditor } from "./doc-editor.tsx";
import { byline } from "./docs.ts";

type Props = { api: Api; workspace: WorkspaceId; socket: WorkspaceSocket; names: Names; doc: DocId; back(): void };

/**
 * One doc, read again as it is revised, with who made its latest revision, and Edit. An edit holds
 * the version it began from, so a revision saved meanwhile is reported and the edit kept, to save
 * over that revision or discard.
 */
export function DocView({ api, workspace, socket, names, doc, back }: Props) {
  const record = useLoaded(() => api.doc(workspace, doc), `${workspace} ${doc}`);
  const history = useLoaded(() => api.docHistory(workspace, doc), `${workspace} ${doc}`);
  const reload = useBurst(() => {
    void record.load();
    void history.load();
  });
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (message.type === "workspace" && message.change.kind === "doc_changed" && message.change.doc === doc) {
          reload();
        }
      }),
    [socket, doc],
  );
  const [draft, setDraft] = useState<{ content: DocContent; version: number }>();
  const [stale, setStale] = useState(false);
  const saving = useAsk();
  const close = () => {
    saving.clear();
    setStale(false);
    setDraft(undefined);
  };
  const save = (content: DocContent, version: number) =>
    void saving.ask(
      () => api.reviseDoc(workspace, doc, version, { ...content, title: content.title.trim() }),
      (saved: DocRecord) => {
        close();
        record.change(() => saved);
        void history.load();
      },
      (code) => {
        if (code === "doc.stale") {
          setStale(true);
          void record.load();
        }
      },
    );
  const current = record.value;
  return (
    <div className="page docs">
      <button type="button" onClick={back}>
        All docs
      </button>
      {record.loading && <p className="muted">Loading</p>}
      {record.problem !== undefined && <Problem text={record.problem} retry={() => void record.load()} />}
      {current !== undefined && draft === undefined && (
        <article>
          <h2>{current.content.title}</h2>
          <p className="muted">{byline(history.value?.revisions[0], names)}</p>
          <div className="text">
            <MarkdownView source={current.content.body} />
          </div>
          <button type="button" onClick={() => setDraft({ content: current.content, version: current.version })}>
            Edit
          </button>
        </article>
      )}
      {current !== undefined && draft !== undefined && (
        <DocEditor initial={draft.content} busy={saving.busy} problem={saving.problem} edited={(content) => setDraft((held) => held && { ...held, content })} save={(content) => save(content, draft.version)} cancel={close}>
          {stale && (
            <div className="stale">
              <p>Your text is still here. Here is how theirs differs from yours:</p>
              <DiffLines before={current.content.body} after={draft.content.body} label="Their text and yours" />
              <div className="actions">
                <button type="button" disabled={saving.busy} onClick={() => save(draft.content, current.version)}>
                  Save mine over theirs
                </button>
                <button type="button" onClick={close}>
                  Discard mine
                </button>
              </div>
            </div>
          )}
        </DocEditor>
      )}
    </div>
  );
}
