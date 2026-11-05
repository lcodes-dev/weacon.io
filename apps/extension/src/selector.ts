/** The parts of a DOM element a selector is built from. */
export type SelectorNode = {
  readonly id: string;
  readonly localName: string;
  readonly parentElement: SelectorNode | null;
  readonly children: ArrayLike<SelectorNode>;
};

/**
 * A CSS selector for `element`: its nearest ancestor with a plain id, then each step down by tag
 * name, numbered among siblings of the same tag where it needs to be.
 */
export function cssSelector(element: SelectorNode): string {
  const steps: string[] = [];
  for (let node: SelectorNode | null = element; node !== null; node = node.parentElement) {
    if (/^[A-Za-z][\w-]*$/.test(node.id)) {
      steps.unshift(`#${node.id}`);
      break;
    }
    steps.unshift(step(node));
  }
  return steps.join(" > ");
}

function step(node: SelectorNode): string {
  const siblings = Array.from(node.parentElement?.children ?? []).filter(
    (sibling) => sibling.localName === node.localName,
  );
  return siblings.length > 1 ? `${node.localName}:nth-of-type(${siblings.indexOf(node) + 1})` : node.localName;
}
