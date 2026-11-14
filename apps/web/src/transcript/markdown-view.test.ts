import assert from "node:assert/strict";
import { test } from "node:test";

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { MarkdownView } from "./markdown-view.ts";

const render = (source: string) => renderToStaticMarkup(createElement(MarkdownView, { source }));

test("each kind of block and inline becomes its element", () => {
  assert.equal(
    render("# Plan\n\nRead **all** of *it* in `a.rs`.\n\n- one\n- two\n\n3. three\n4. four\n\n```\nlet x = 1;\n```"),
    '<p class="heading">Plan</p>' +
      "<p>Read <strong>all</strong> of <em>it</em> in <code>a.rs</code>.</p>" +
      "<ul><li>one</li><li>two</li></ul>" +
      '<ol start="3"><li>three</li><li>four</li></ol>' +
      "<pre><code>let x = 1;</code></pre>",
  );
});

test("markup in the text stays text", () => {
  assert.equal(
    render('<img src=x onerror="alert(1)"> **<script>x</script>**'),
    "<p>&lt;img src=x onerror=&quot;alert(1)&quot;&gt; <strong>&lt;script&gt;x&lt;/script&gt;</strong></p>",
  );
});
