import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { api, type Session } from "./api"
import "./app.css"
import { Shell } from "./Shell"
import { SignIn } from "./SignIn"

function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: () => api<Session>("GET", "/session") })
  if (!session.data) return null
  if (!session.data.user) return <SignIn setup={session.data.setup_needed} />
  return <Shell user={session.data.user} />
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
)
