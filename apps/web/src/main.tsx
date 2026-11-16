import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { ChatPage } from "./chat-page/chat-page.tsx";
import { chatRefOf } from "./chat-ref.ts";
import { fakeServerUrl } from "./server.ts";
import { Shell } from "./shell/shell.tsx";

const fake = fakeServerUrl(window.location);
const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      {fake === undefined ? <Shell fetch={fetch.bind(window)} /> : <ChatPage url={fake} chatRef={chatRefOf(window.location.search)} />}
    </StrictMode>,
  );
}
