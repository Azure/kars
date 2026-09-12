export function modelKey(provider: string, deployment: string) {
  return `${provider}::${deployment}`;
}

export function moveFallback(routes: string[], index: number, delta: number): string[] {
  const next = index + delta;
  if (next < 0 || next >= routes.length) return routes;
  const copy = [...routes];
  [copy[index], copy[next]] = [copy[next], copy[index]];
  return copy;
}
