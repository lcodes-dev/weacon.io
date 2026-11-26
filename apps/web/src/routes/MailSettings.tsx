import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { api, type MailConfig, type Smtp } from "../api"

const empty: Smtp = { host: "", port: 587, security: "starttls", username: null, from: "" }

export function MailSettings() {
  const config = useQuery({ queryKey: ["mail-config"], queryFn: () => api<MailConfig>("GET", "/config/mail") })
  return config.data ? <Form saved={config.data.smtp} /> : null
}

function Form({ saved }: { saved: Smtp | null }) {
  const queryClient = useQueryClient()
  const [smtp, setSmtp] = useState(saved ?? empty)
  const [password, setPassword] = useState("")
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["mail-config"] })

  const save = useMutation({
    mutationFn: () => api("PUT", "/config/mail", { ...smtp, password: password || undefined }),
    onSuccess: refresh,
  })
  const clear = useMutation({ mutationFn: () => api("DELETE", "/config/mail"), onSuccess: refresh })
  const test = useMutation({ mutationFn: () => api("POST", "/config/mail/test") })
  const error = save.error ?? clear.error ?? test.error
  const set = (change: Partial<Smtp>) => setSmtp({ ...smtp, ...change })

  return (
    <div className="page">
      <h1>Mail settings</h1>
      <p className="text-ink2">
        {saved
          ? `Mail goes through ${saved.host}.`
          : "Mail isn't set up. Until it is, every mail is only printed to the server's log."}
      </p>
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault()
          test.reset()
          save.mutate()
        }}
      >
        <div className="grid grid-cols-[1fr_90px_110px] gap-2 phone:grid-cols-1">
          <Field>
            <FieldLabel>SMTP server</FieldLabel>
            <Input value={smtp.host} onChange={(event) => set({ host: event.target.value })} required />
          </Field>
          <Field>
            <FieldLabel>Port</FieldLabel>
            <Input
              type="number"
              value={String(smtp.port)}
              onChange={(event) => set({ port: Number(event.target.value) })}
              required
            />
          </Field>
          <Field>
            <FieldLabel>Security</FieldLabel>
            <FieldControl
              render={<select />}
              value={smtp.security}
              onChange={(event) => set({ security: event.target.value as Smtp["security"] })}
            >
              <option value="starttls">STARTTLS</option>
              <option value="tls">TLS</option>
              <option value="none">None</option>
            </FieldControl>
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-2 phone:grid-cols-1">
          <Field>
            <FieldLabel>Username</FieldLabel>
            <Input value={smtp.username ?? ""} onChange={(event) => set({ username: event.target.value || null })} />
          </Field>
          <Field>
            <FieldLabel>Password</FieldLabel>
            <Input
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              placeholder={saved ? "Unchanged" : ""}
            />
          </Field>
        </div>
        <Field>
          <FieldLabel>Send mail as</FieldLabel>
          <Input
            value={smtp.from}
            onChange={(event) => set({ from: event.target.value })}
            placeholder="Weacon <weacon@example.com>"
            required
          />
        </Field>
        <div className="flex flex-wrap gap-2">
          <button className="btn btn-primary" disabled={save.isPending}>
            Save
          </button>
          {saved && (
            <>
              <button type="button" className="btn" disabled={test.isPending} onClick={() => test.mutate()}>
                Send a test mail
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => {
                  test.reset()
                  clear.mutate()
                }}
              >
                Stop using SMTP
              </button>
            </>
          )}
        </div>
      </form>
      {test.isSuccess && <p className="text-ok">Test mail sent. Check your inbox.</p>}
      {error && <p className="text-acc">{error.message}</p>}
    </div>
  )
}
