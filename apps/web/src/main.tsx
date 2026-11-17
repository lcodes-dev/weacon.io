import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { BrowserRouter, Route, Routes } from "react-router"
import { api, type Session } from "./api"
import "./app.css"
import { Invite } from "./Invite"
import { Shell } from "./Shell"
import { SignIn } from "./SignIn"

function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: () => api<Session>("GET", "/session") })
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
