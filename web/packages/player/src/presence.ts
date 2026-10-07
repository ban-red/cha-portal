// Whether this page is using the environment, for the node's idle shutoff: the page is in front,
// or its sound is playing to someone. A hidden tab that is silent or muted is a forgotten one.

export function presenceActive(visible: boolean, audible: boolean): boolean {
  return visible || audible;
}
