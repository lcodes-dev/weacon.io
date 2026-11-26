import { useQuery } from "@tanstack/react-query"
import { api, type Chat, type Github, type Group, type Machine, type MailConfig, type Project, type Task, type Team, type TeamDetails } from "./api"

export function useTeamDetails(team: Team) {
  return useQuery({ queryKey: ["team", team.id], queryFn: () => api<TeamDetails>("GET", `/teams/${team.id}`) })
}

export function useProjects(team: Team) {
  return useQuery({ queryKey: ["projects", team.id], queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`) })
}

export function useTasks(team: Team, project?: string) {
  return useQuery({
    queryKey: ["tasks", project],
    queryFn: () => api<Task[]>("GET", `/teams/${team.id}/projects/${project}/tasks`),
    enabled: !!project,
  })
}

export function useChats(team: Team) {
  return useQuery({ queryKey: ["chats", team.id], queryFn: () => api<Chat[]>("GET", `/teams/${team.id}/chats`) })
}

export function useGroups(team: Team) {
  return useQuery({ queryKey: ["groups", team.id], queryFn: () => api<Group[]>("GET", `/teams/${team.id}/groups`) })
}

export function useMachines() {
  return useQuery({ queryKey: ["machines"], queryFn: () => api<Machine[]>("GET", "/machines") })
}

export function useGithub(team: Team) {
  return useQuery({ queryKey: ["github", team.id], queryFn: () => api<Github>("GET", `/teams/${team.id}/github`) })
}

export function useMailConfig() {
  return useQuery({ queryKey: ["mail-config"], queryFn: () => api<MailConfig>("GET", "/config/mail") })
}
