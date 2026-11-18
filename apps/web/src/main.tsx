import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { BrowserRouter, Route, Routes } from "react-router"
import { api, type Session } from "./api"
import "./app.css"
import { Shell } from "./components/Shell"
import { Invite } from "./routes/Invite"
import { SignIn } from "./routes/SignIn"

function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: () => api<Session>("GET", "/session") })
  if (session.error)
    return (
      <div className="page flex flex-col items-start gap-2">
        <p>Weacon can't be reached.</p>
        <button className="btn" onClick={() => session.refetch()}>
          Try again
        </button>
      </div>
    )
  if (!session.data) return null
  const { user, setup_needed } = session.data
  return (
    <Routes>
      <Route path="/invite/:token" element={<Invite user={user} />} />
      <Route path="*" element={user ? <Shell user={user} /> : <SignIn setup={setup_needed} />} />
    </Routes>
  )
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
)
