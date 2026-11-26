import { hierarchy, pack, type HierarchyCircularNode } from "d3-hierarchy"
import { useEffect, useMemo, useRef, useState } from "react"
import type { MapFile } from "../api"

type Node = { name: string; path: string; file?: MapFile; children: Node[] }

function tree(files: MapFile[]): Node {
  const root: Node = { name: "", path: "", children: [] }
  for (const file of files) {
    let folder = root
    const parts = file.path.split("/")
    parts.forEach((name, index) => {
      const path = parts.slice(0, index + 1).join("/")
      let child = folder.children.find((child) => child.name === name)
      if (!child) {
        child = { name, path, children: [], file: index === parts.length - 1 ? file : undefined }
        folder.children.push(child)
      }
      folder = child
    })
  }
  return root
}

const width = 900
const height = 700
type Circle = HierarchyCircularNode<Node>

function layout(files: MapFile[], uses: [string, string][]) {
  const size = (file: MapFile) => (file.language ? Math.max(Math.min(file.lines, 400), 30) : 14)
  const sized = hierarchy(tree(files)).sum((node) => (node.file ? size(node.file) : 0))
  const root = pack<Node>().size([width, height]).padding((node) => (node.depth === 0 ? 14 : 5))(sized)
  const at = new Map(root.descendants().map((circle) => [circle.data.path, circle]))
  const links = uses.flatMap(([from, to]) => {
    const [a, b] = [at.get(from), at.get(to)]
    if (!a || !b) return []
    const shared = a.path(b).reduce((top, circle) => (circle.depth < top.depth ? circle : top))
    return { from, to, d: `M${a.x},${a.y}Q${shared.x},${shared.y} ${b.x},${b.y}` }
  })
  return { root, at, links }
}

export function CodeGraph(props: {
  files: MapFile[]
  uses: [string, string][]
  colors: Record<string, string>
  folder: string
  onEnter: (folder: string) => void
  selected: string
  onSelect: (path: string) => void
}) {
  const { files, uses, colors, folder, onEnter, selected, onSelect } = props
  const { root, at, links } = useMemo(() => layout(files, uses), [files, uses])
  const focus = at.get(folder) ?? root
  const svg = useRef<SVGSVGElement>(null)
  const fit = (circle: Circle) => {
    const zoom = Math.min(width, height) / (circle.r * 2 + 8)
    return { zoom, x: width / 2 - circle.x * zoom, y: height / 2 - circle.y * zoom, glide: true }
  }
  const [view, setView] = useState(() => fit(focus))
  const { zoom } = view
  const dragged = useRef(false)
  useEffect(() => setView(fit(focus)), [focus])

  useEffect(() => {
    const element = svg.current!
    const wheel = (event: WheelEvent) => {
      event.preventDefault()
      const box = element.getBoundingClientRect()
      const [px, py] = [((event.clientX - box.left) * width) / box.width, ((event.clientY - box.top) * height) / box.height]
      setView((view) => {
        const zoom = Math.max(0.6, Math.min(60, view.zoom * Math.exp(-event.deltaY * 0.002)))
        const by = zoom / view.zoom
        return { zoom, x: px - (px - view.x) * by, y: py - (py - view.y) * by, glide: false }
      })
    }
    element.addEventListener("wheel", wheel, { passive: false })
    return () => element.removeEventListener("wheel", wheel)
  }, [])
  const drag = (event: React.PointerEvent<SVGSVGElement>) => {
    if (event.buttons !== 1 || (!event.movementX && !event.movementY)) return
    dragged.current = true
    const scale = width / event.currentTarget.getBoundingClientRect().width
    setView((view) => ({ ...view, x: view.x + event.movementX * scale, y: view.y + event.movementY * scale, glide: false }))
  }
  const onScreen = (circle: Circle) => circle.r * 2 * zoom
  const linked = new Set(links.filter((link) => link.from === selected || link.to === selected).flatMap((link) => [link.from, link.to]))

  const click = (circle: Circle) => {
    if (dragged.current) return
    if (circle.data.file) return onSelect(circle.data.path === selected ? "" : circle.data.path)
    const step = circle.ancestors().find((ancestor) => ancestor.parent === focus)
    onEnter((step ?? focus.parent ?? root).data.path)
  }
  const label = (circle: Circle) => {
    const { name, file } = circle.data
    const room = onScreen(circle) / 6.2
    if (file) return room >= 5 ? (name.length > room ? `${name.slice(0, room - 1)}…` : name) : ""
    return name.length <= room && onScreen(circle) < width ? name : ""
  }

  return (
    <svg
      ref={svg}
      viewBox={`0 0 ${width} ${height}`}
      className="min-w-0 flex-1 cursor-grab touch-none rounded-md border border-line active:cursor-grabbing"
      role="img"
      aria-label="Code graph"
      onPointerDown={() => (dragged.current = false)}
      onPointerMove={drag}
      onClick={() => !dragged.current && onEnter((focus.parent ?? root).data.path)}
    >
      <g
        style={{ transition: view.glide ? "transform 450ms ease" : undefined }}
        transform={`translate(${view.x},${view.y}) scale(${zoom})`}
      >
        {root.descendants().map((circle) => {
          const { file, path } = circle.data
          if (!file && circle !== root) {
            return (
              <circle
                key={path}
                cx={circle.x}
                cy={circle.y}
                r={circle.r}
                fill="var(--ink)"
                fillOpacity={circle.depth % 2 ? 0.05 : 0.02}
                stroke="var(--fld)"
                vectorEffect="non-scaling-stroke"
                onClick={(event) => {
                  event.stopPropagation()
                  click(circle)
                }}
              >
                <title>{path}</title>
              </circle>
            )
          }
          return null
        })}
        {links.map((link) => {
          const mine = link.from === selected || link.to === selected
          return (
            <path
              key={`${link.from}\n${link.to}`}
              d={link.d}
              fill="none"
              stroke={mine ? "var(--ink)" : "var(--ink3)"}
              strokeOpacity={mine ? 0.9 : selected ? 0.05 : 0.18}
              strokeWidth={mine ? 1.6 : 1}
              strokeDasharray={mine && link.to === selected ? "4 3" : undefined}
              vectorEffect="non-scaling-stroke"
              pointerEvents="none"
            />
          )
        })}
        {root.leaves().map((circle) => {
          const { file, path } = circle.data
          const faded = selected && path !== selected && !linked.has(path)
          return (
            <circle
              key={path}
              cx={circle.x}
              cy={circle.y}
              r={circle.r}
              fill={colors[file?.language ?? ""] ?? "var(--fld)"}
              fillOpacity={faded ? 0.2 : 1}
              stroke={path === selected ? "var(--ink)" : "var(--bg)"}
              strokeWidth={path === selected ? 2.5 : 1}
              vectorEffect="non-scaling-stroke"
              onClick={(event) => {
                event.stopPropagation()
                click(circle)
              }}
            >
              <title>{`${path} · ${file?.language ?? "not read"} · ${file?.lines} lines`}</title>
            </circle>
          )
        })}
        {root.descendants().map((circle) => {
          const text = label(circle)
          if (!text) return null
          const folderLabel = !circle.data.file
          return (
            <text
              key={circle.data.path}
              x={circle.x}
              y={folderLabel ? circle.y - circle.r + 13 / zoom : circle.y + 3.5 / zoom}
              fontSize={(folderLabel ? 12 : 10) / zoom}
              fontWeight={folderLabel ? 600 : 400}
              textAnchor="middle"
              fill="var(--ink)"
              stroke="var(--bg)"
              strokeWidth={folderLabel ? 3 / zoom : 0}
              paintOrder="stroke"
              pointerEvents="none"
            >
              {text}
            </text>
          )
        })}
      </g>
    </svg>
  )
}
