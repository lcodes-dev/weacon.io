import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useNavigate, useParams } from "react-router"
import { api, type Invitation, type User } from "./api"
import { SignIn } from "./SignIn"

/** The screen an invite link opens. */
export function Invite({ user }: { user: User | null }) {
  const { token } = useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const invited = useQuery({
    queryKey: ["invite", token],
    queryFn: () => api<Invitation>("GET", `/invites/${token}`),
    retry: false,
  })
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["session"] })
  const join = useMutation({
    mutationFn: () => api<{ team_id: string }>("POST", `/invites/${token}/accept`),
    onSuccess: async ({ team_id }) => {
      localStorage.setItem("team", team_id)
      await refresh()
      navigate("/")
    },
  })
  const signOut = useMutation({ mutationFn: () => api("POST", "/sign-out"), onSuccess: refresh })

  if (invited.isPending) return null
  const invitation = invited.data
  if (invitation && !user) return <SignIn setup={false} invitation={invitation} />
  const forSomeoneElse = invitation && invitation.email !== user?.email
  const error = invited.error ?? join.error

  return (
    <div className="flex h-screen items-center justify-center">
      <div className="flex w-[340px] flex-col gap-4 rounded-xl border border-line bg-sur p-6">
        {invitation && (
          <>
            <h1 className="text-[17px] font-semibold">Join {invitation.team_name}</h1>
            <p className="text-ink2">
              {invitation.invited_by} invited you to {invitation.team_name}.
            </p>
          </>
        )}
        {forSomeoneElse && (
          <p className="text-ink2">
            This invite is for {invitation.email}, and you are signed in as {user?.email}.
          </p>
        )}
        {error && <p className="text-acc">{error.message}</p>}
        <div className="flex gap-2">
          {forSomeoneElse && (
            <button className="btn btn-primary" onClick={() => signOut.mutate()}>
              Sign out
            </button>
          )}
          {invitation && !forSomeoneElse && (
            <button className="btn btn-primary" disabled={join.isPending} onClick={() => join.mutate()}>
              Join
            </button>
          )}
          <button className="btn" onClick={() => navigate("/")}>
            {invitation ? "Not now" : "Continue"}
          </button>
        </div>
      </div>
    </div>
  )
}
