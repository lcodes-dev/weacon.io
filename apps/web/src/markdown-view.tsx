import type { ReactNode } from "react";

import { type Block, type Inline, blocks } from "./markdown.ts";

/** `source` as Markdown, built from elements so nothing in it is read as HTML. */
export function MarkdownView({ source }: { source: string }) {
  return <>{blocks(source).map(block)}</>;
}

function block(item: Block, at: number): ReactNode {
  switch (item.kind) {
    case "paragraph":
      return <p key={at}>{item.inlines.map(inline)}</p>;
    case "heading":
      return (
        <p key={at} className="heading">
          {item.inlines.map(inline)}
        </p>
      );
    case "list": {
      const items = item.items.map((parts, index) => <li key={index}>{parts.map(inline)}</li>);
      return item.start === undefined ? (
        <ul key={at}>{items}</ul>
      ) : (
        <ol key={at} start={item.start}>
          {items}
        </ol>
      );
    }
    case "code":
      return (
        <pre key={at}>
          <code>{item.text}</code>
        </pre>
      );
  }
}

function inline(part: Inline, at: number): ReactNode {
  switch (part.kind) {
    case "text":
      return part.text;
    case "code":
      return <code key={at}>{part.text}</code>;
    case "strong":
      return <strong key={at}>{part.text}</strong>;
    case "em":
      return <em key={at}>{part.text}</em>;
  }
}
