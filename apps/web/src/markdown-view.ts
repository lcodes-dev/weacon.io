import { type ReactNode, createElement as h } from "react";

import { type Block, type Inline, blocks } from "./markdown.ts";

/** `source` as Markdown, built from elements so nothing in it is read as HTML. */
export function MarkdownView({ source }: { source: string }): ReactNode {
  return blocks(source).map(block);
}

function block(item: Block, at: number): ReactNode {
  switch (item.kind) {
    case "paragraph":
      return h("p", { key: at }, ...item.inlines.map(inline));
    case "heading":
      return h("p", { key: at, className: "heading" }, ...item.inlines.map(inline));
    case "list": {
      const items = item.items.map((parts, index) => h("li", { key: index }, ...parts.map(inline)));
      return item.start === undefined ? h("ul", { key: at }, items) : h("ol", { key: at, start: item.start }, items);
    }
    case "code":
      return h("pre", { key: at }, h("code", null, item.text));
  }
}

function inline(part: Inline, at: number): ReactNode {
  switch (part.kind) {
    case "text":
      return part.text;
    case "code":
      return h("code", { key: at }, part.text);
    case "strong":
      return h("strong", { key: at }, part.text);
    case "em":
      return h("em", { key: at }, part.text);
  }
}
