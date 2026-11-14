import {
  type Dispatch,
  Fragment,
  type MouseEvent,
  type RefObject,
  type SetStateAction,
  memo,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import "./diff.css";

import type { DiffFile } from "../protocol.ts";
import { type Changes, type DiffRow, rows } from "./changes.ts";
import { type Comment, type Selection, addComment, editComment, selectRows, removeComment, span } from "./comments.ts";
import { changeText, contentText, hunkText, linesText, summary } from "./labels.ts";
import { CommentBox, CommentView } from "./review.tsx";

// Keep in step with `.diff .lines` in diff.css; an off-screen file reserves this much per row.
const ROW_PX = 20;

/**
 * The lines being picked, and where the comment box is. The box moves to the picked lines only
 * once the pointer lets go, so nothing moves under a pressed pointer.
 */
type Picking = { live: Selection | undefined; box: Selection | undefined };

const NOT_PICKING: Picking = { live: undefined, box: undefined };

/** How a file changes the page's picking, draft and comments; the same object for every render. */
type Review = {
  setPicking: Dispatch<SetStateAction<Picking>>;
  setDraft: Dispatch<SetStateAction<string>>;
  setComments: Dispatch<SetStateAction<readonly Comment[]>>;
  /** The file whose lines the pointer is dragging over, if any. */
  dragging: RefObject<string | undefined>;
};

/** A chat's changed files, one section each, as their answers arrive, with the comments on them. */
export function DiffView({
  changes,
  comments,
  setComments,
}: {
  changes: Changes;
  comments: readonly Comment[];
  setComments: Dispatch<SetStateAction<readonly Comment[]>>;
}) {
  const [picking, setPicking] = useState(NOT_PICKING);
  const [draft, setDraft] = useState("");
  const dragging = useRef<string>(undefined);
  const review = useMemo<Review>(() => ({ setPicking, setDraft, setComments, dragging }), [setComments]);
  const byPath = useMemo(() => Map.groupBy(comments, (comment) => comment.path), [comments]);
  useEffect(() => {
    const stop = () => {
      if (dragging.current !== undefined) {
        dragging.current = undefined;
        setPicking(({ live }) => ({ live, box: live }));
      }
    };
    window.addEventListener("mouseup", stop);
    return () => window.removeEventListener("mouseup", stop);
  }, []);

  if (changes.files.length === 0) {
    return <p className="page muted">{summary(changes)}</p>;
  }
  const { live, box } = picking;
  return (
    <div className="page diff">
      <p className="muted">{summary(changes)}</p>
      {changes.files.map((file, at) => (
        <FileView
          key={at}
          file={file}
          comments={byPath.get(file.path) ?? NONE}
          live={live?.path === file.path ? live : undefined}
          box={box?.path === file.path ? box : undefined}
          draft={box?.path === file.path ? draft : ""}
          review={review}
        />
      ))}
    </div>
  );
}

const NONE: readonly Comment[] = [];

const FileView = memo(function FileView({
  file,
  comments,
  live,
  box,
  draft,
  review,
}: {
  file: DiffFile;
  comments: readonly Comment[];
  live: Selection | undefined;
  box: Selection | undefined;
  draft: string;
  review: Review;
}) {
  const { content, path } = file;
  const note = contentText(content);
  const lines = useMemo(() => (note === undefined && content.kind === "text" ? rows(content.hunks) : []), [note, content]);
  const picked = live && span(live);
  const boxAt = box && span(box);
  const marked = (at: number) =>
    picked && at >= picked.first && at <= picked.last
      ? "selected"
      : comments.some((comment) => at >= comment.first && at <= comment.last)
        ? "commented"
        : undefined;
  const close = () => {
    review.setPicking(NOT_PICKING);
    review.setDraft("");
  };
  const after = (at: number) => (
    <>
      {comments
        .filter((comment) => comment.last === at)
        .map((comment) => (
          <CommentView
            key={comment.id}
            comment={comment}
            label={linesText(lines, comment.first, comment.last)}
            onEdit={(text) => review.setComments((before) => editComment(before, comment.id, text))}
            onRemove={() => review.setComments((before) => removeComment(before, comment.id))}
          />
        ))}
      {box && boxAt?.last === at && (
        <CommentBox
          label={linesText(lines, boxAt.first, boxAt.last)}
          text={draft}
          action="Comment"
          onChange={review.setDraft}
          onSave={() => {
            review.setComments((before) => addComment(before, box, draft));
            close();
          }}
          onCancel={close}
        />
      )}
    </>
  );
  return (
    <section className="file" style={{ containIntrinsicSize: `auto ${(lines.length + 2) * ROW_PX}px` }}>
      <h2>
        <span className="path">{path}</span>
        <span className="muted">{changeText(file.change)}</span>
      </h2>
      {note === undefined ? (
        <div className="lines" onMouseDown={(event) => press(event, path, review)} onMouseOver={(event) => drag(event, path, review)}>
          {lines.map((row, at) => (
            <Fragment key={at}>
              <RowView row={row} at={at} mark={marked(at)} />
              {after(at)}
            </Fragment>
          ))}
        </div>
      ) : (
        <p className="muted">{note}</p>
      )}
    </section>
  );
});

const RowView = memo(function RowView({ row, at, mark }: { row: DiffRow; at: number; mark: "selected" | "commented" | undefined }) {
  if (row.kind === "hunk") {
    return <div className="note">{hunkText(row.old, row.new)}</div>;
  }
  return (
    <>
      <div className="line" data-row={at} data-op={row.op} data-old={row.old} data-new={row.new} data-mark={mark}>
        <span>{row.text}</span>
      </div>
      {row.noNewline && <div className="note">No newline at the end of the file.</div>}
    </>
  );
});

function rowOf(line: HTMLElement): number {
  return Number(line.dataset["row"]);
}

// The text is in a span, so only a press beside it, on the gutter, picks the line.
function press(event: MouseEvent, path: string, review: Review): void {
  const target = event.target;
  if (event.button !== 0 || !(target instanceof HTMLElement) || !target.classList.contains("line")) {
    return;
  }
  event.preventDefault();
  review.dragging.current = path;
  review.setPicking(({ live, box }) => ({ live: selectRows(live, path, rowOf(target), event.shiftKey), box }));
}

function drag(event: MouseEvent, path: string, review: Review): void {
  const line = event.target instanceof Element ? event.target.closest<HTMLElement>(".line") : null;
  if (review.dragging.current !== path || line === null) {
    return;
  }
  // The release happened outside the page.
  if ((event.buttons & 1) === 0) {
    review.dragging.current = undefined;
    review.setPicking(({ live }) => ({ live, box: live }));
    return;
  }
  review.setPicking(({ live, box }) => ({ live: selectRows(live, path, rowOf(line), true), box }));
}
