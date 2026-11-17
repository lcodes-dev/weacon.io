export type Health = { status: "ok" }

export async function get<T>(path: string): Promise<T> {
  const response = await fetch(`/api${path}`)
  if (!response.ok) throw new Error(`GET /api${path}: ${response.status}`)
  return response.json()
}
