import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { api, type CodeMap as Map, type Project, type Team } from "../api"
import { CodeGraph } from "./CodeGraph"

const colors: Record<string, string> = {
  TypeScript: "var(--lang-1)",
  Rust: "var(--lang-2)",
  Python: "var(--lang-3)",
  JavaScript: "var(--lang-4)",
  PHP: "var(--lang-5)",
}

/** A branch's map: the graph, the trail of the folder in view, and a panel for the selected file. */
export function CodeMap({ team, project }: { team: Team; project: Project }) {
  const mapped = project.branches.filter((branch) => branch.commit_sha)
  const [chosen, setChosen] = useState("")
  const [folder, setFolder] = useState("")
  const [selected, setSelected] = useState("")
  const branch = mapped.find((branch) => branch.name === chosen) ?? mapped[0]
  const map = useQuery({
    // The commit is in the key so that the picture follows the branch.
    queryKey: ["map", project.id, branch?.name, branch?.commit_sha],
    queryFn: () => api<Map>("GET", `/teams/${team.id}/projects/${project.id}/map?branch=${encodeURIComponent(branch.name)}`),
    enabled: !!branch,
  })
  if (!branch || !map.data) return null

  const { files, uses } = map.data
  const file = files.find((file) => file.path === selected)
  const usedBySelected = uses.filter(([from]) => from === selected).map(([, to]) => to)
  const usersOfSelected = uses.filter(([, to]) => to === selected).map(([from]) => from)
  const trail = folder ? folder.split("/") : []
  const languages = [...new Set(files.map((file) => file.language ?? "Other"))].sort()

  const enter = (path: string) => {
    setFolder(path)
    setSelected("")
  }
  const fileList = (title: string, paths: string[]) =>
    paths.length > 0 && (
      <>
        <h3 className="pt-2 text-ink2">{title}</h3>
        {paths.map((path) => (
          <button key={path} className="truncate text-left underline" onClick={() => setSelected(path)}>
            {path}
          </button>
        ))}
      </>
    )

  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="mr-auto">Code map</h2>
        {mapped.length > 1 && (
          <select
            aria-label="Branch"
            className="rounded-md border border-fld bg-bg px-2 py-1"
            value={branch.name}
            onChange={(event) => setChosen(event.target.value)}
          >
            {mapped.map((branch) => (
              <option key={branch.name}>{branch.name}</option>
            ))}
          </select>
        )}
        <span className="text-ink2">
          {mapped.length === 1 && `${branch.name} · `}commit {branch.commit_sha?.slice(0, 7)} · {files.length} files
        </span>
      </div>
      <nav className="flex flex-wrap gap-1 text-ink2">
        <button className="underline" onClick={() => enter("")}>
          {project.name}
        </button>
        {trail.map((name, index) => (
          <button key={index} className="underline" onClick={() => enter(trail.slice(0, index + 1).join("/"))}>
            / {name}
          </button>
        ))}
      </nav>
      <div className="flex items-start gap-3 phone:flex-col phone:items-stretch">
        <CodeGraph
          files={files}
          uses={uses}
          colors={colors}
          folder={folder}
          onEnter={enter}
          selected={selected}
          onSelect={setSelected}
        />
        <aside className="flex w-[220px] shrink-0 flex-col gap-1 phone:w-full">
          {file ? (
            <>
              <div className="font-medium break-all">{file.path}</div>
              <div className="text-ink2">
                {file.language ?? "Not read"} · {file.lines} lines
              </div>
              {file.symbols.map((symbol, index) => (
                <a
                  // A minified file defines the same name many times on one line.
                  key={index}
                  className="flex gap-2 hover:underline"
                  href={`https://github.com/${project.repo}/blob/${map.data.commit}/${file.path}#L${symbol.line}`}
                  target="_blank"
                  rel="noreferrer"
                >
                  <span className="w-[62px] shrink-0 text-ink2">{symbol.kind}</span>
                  <span className="min-w-0 flex-1 truncate">{symbol.name}</span>
                  <span className="text-ink2">{symbol.line}</span>
                </a>
              ))}
              {fileList("Uses", usedBySelected)}
              {fileList("Used by", usersOfSelected)}
            </>
          ) : (
            <>
              <p className="text-ink2">
                A dot is a file, inside the circle of its folder. A line joins a file to one it uses. Scroll to zoom
                and drag to move. Click a folder to go into it and outside it to come back; click a file to see it,
                with solid lines to what it uses and dashed lines from what uses it.
              </p>
              {languages.map((language) => (
                <div key={language} className="flex items-center gap-2">
                  <span className="size-3 rounded-full" style={{ background: colors[language] ?? "var(--fld)" }} />
                  {language}
                </div>
              ))}
            </>
          )}
        </aside>
      </div>
    </>
  )
}
