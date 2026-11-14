import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { ChatPage } from "./chat-page/chat-page.tsx";
import { Log } from "./log.ts";
import { fakeServerUrl } from "./server.ts";
import { Shell } from "./shell/shell.tsx";

const log = new Log();
// A crash is reported by a fixed code, as its message and stack could hold content, and once,
// as an error in a loop would repeat it for every frame.
let crashed = false;
const crash = () => {
  if (!crashed) {
    crashed = true;
    log.report("web.crashed");
  }
};
window.addEventListener("error", crash);
window.addEventListener("unhandledrejection", crash);
log.info("app started");
const fake = fakeServerUrl(window.location);
const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      {fake === undefined ? <Shell fetch={fetch.bind(window)} /> : <ChatPage url={fake} log={log} />}
    </StrictMode>,
  );
}
