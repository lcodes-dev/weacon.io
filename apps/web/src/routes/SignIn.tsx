import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { OTPInput, OTPInputGroup, OTPInputSlot } from "@ui-sdk/react/otp-input"
import { useState } from "react"
import { api, type Invitation } from "../api"

export function SignIn({ setup, invitation }: { setup: boolean; invitation?: Invitation }) {
  const queryClient = useQueryClient()
  const [registering, setRegistering] = useState(setup || invitation?.has_account === false)
  const [name, setName] = useState("")
  const [email, setEmail] = useState(invitation?.email ?? "")

  const sendCode = useMutation({
    mutationFn: () => api("POST", "/sign-in/code", { email, name: registering ? name : undefined }),
  })
  const verify = useMutation({
    mutationFn: (code: string) => api("POST", "/sign-in/verify", { email, code }),
    // What an account that was signed in before left behind goes too.
    onSuccess: () => queryClient.resetQueries(),
  })

  const title = setup ? "Set up Weacon" : registering ? "Create an account" : "Sign in"
  const error = sendCode.error ?? verify.error

  return (
    <div className="flex h-screen items-center justify-center">
      <div className="flex w-[340px] flex-col gap-4 rounded-xl border border-line bg-sur p-6">
        <h1 className="text-[17px] font-semibold">{title}</h1>
        {sendCode.isSuccess ? (
          <>
            <p className="text-ink2">
              {setup
                ? "Mail isn't set up yet, so your 6-digit code is in the server's log."
                : registering
                  ? `We sent a 6-digit code to ${email}.`
                  : `If ${email} has an account, we sent it a 6-digit code.`}
            </p>
            <OTPInput aria-label="Sign-in code" onValueCommit={(code) => verify.mutate(code)}>
              <OTPInputGroup>
                {[0, 1, 2, 3, 4, 5].map((index) => (
                  <OTPInputSlot key={index} index={index} />
                ))}
              </OTPInputGroup>
            </OTPInput>
            <button className="self-start text-ink2 underline" onClick={() => sendCode.reset()}>
              Use another email
            </button>
          </>
        ) : (
          <form
            className="flex flex-col gap-3"
            onSubmit={(event) => {
              event.preventDefault()
              verify.reset()
              sendCode.mutate()
            }}
          >
            {setup && (
              <p className="text-ink2">Create the first account. It owns this server.</p>
            )}
            {invitation && (
              <p className="text-ink2">
                {invitation.invited_by} invited you to {invitation.team_name}. Sign in or create an account with this
                email to join.
              </p>
            )}
            {registering && (
              <Field>
                <FieldLabel>Your name</FieldLabel>
                <Input value={name} onChange={(event) => setName(event.target.value)} autoComplete="name" required />
              </Field>
            )}
            <Field>
              <FieldLabel>Email</FieldLabel>
              <Input
                type="email"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
                autoComplete="email"
                required
              />
            </Field>
            <button className="btn btn-primary" disabled={sendCode.isPending}>
              Send me a code
            </button>
            {!setup && (
              <button type="button" className="self-start text-ink2 underline" onClick={() => setRegistering(!registering)}>
                {registering ? "I already have an account" : "Create an account"}
              </button>
            )}
          </form>
        )}
        {error && <p className="text-acc">{error.message}</p>}
      </div>
    </div>
  )
}
