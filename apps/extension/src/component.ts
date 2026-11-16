/** The component that rendered an element, as the page's framework tells it in development. */
export type Component = { component?: string; source?: string };

/**
 * The component that rendered the element `selector` finds in `root`, where React's development
 * fiber or Vue's component instance names it. Injected into the page's own world, so it uses
 * nothing from outside itself; what it returns is the page's, so it is untrusted and cut short.
 */
export function componentOf(selector: string, root: Pick<Document, "querySelector"> = document): Component | null {
  const MAX = 256;
  const element = root.querySelector(selector) as unknown as Record<string, unknown> | null;
  const field = (from: unknown, key: string): unknown =>
    (typeof from === "object" && from !== null) || typeof from === "function" ? (from as Record<string, unknown>)[key] : undefined;
  const text = (value: unknown) => (typeof value === "string" && value !== "" ? value.slice(0, MAX) : undefined);
  const found = (component: unknown, source: unknown): Component | null => {
    const named: Component = {};
    const [name, file] = [text(component), text(source)];
    if (name !== undefined) {
      named.component = name;
    }
    if (file !== undefined) {
      named.source = file;
    }
    return name === undefined && file === undefined ? null : named;
  };
  if (element === null) {
    return null;
  }
  const fiberKey = Object.keys(element).find((key) => key.startsWith("__reactFiber$"));
  for (let fiber = fiberKey === undefined ? undefined : element[fiberKey], depth = 0; fiber && depth < 64; fiber = field(fiber, "return"), depth += 1) {
    const type = field(fiber, "type");
    if (typeof type === "function" || (typeof type === "object" && type !== null)) {
      return found(field(type, "displayName") ?? field(type, "name"), field(field(fiber, "_debugSource"), "fileName"));
    }
  }
  const vue = field(field(element, "__vueParentComponent"), "type");
  if (vue !== undefined) {
    return found(field(vue, "__name") ?? field(vue, "name"), field(vue, "__file"));
  }
  const options = field(field(element, "__vue__"), "$options");
  return options === undefined ? null : found(field(options, "name"), field(options, "__file"));
}

/**
 * Only the names in what `componentOf` answered from the page's own world, which the page
 * controls: it may answer anything, with any other fields.
 */
export function componentIn(answer: unknown): Component {
  const fields = typeof answer === "object" && answer !== null ? (answer as Record<string, unknown>) : {};
  const named: Component = {};
  if (typeof fields["component"] === "string") {
    named.component = fields["component"];
  }
  if (typeof fields["source"] === "string") {
    named.source = fields["source"];
  }
  return named;
}
