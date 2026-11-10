// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

const originalFetch = window.fetch.bind(window);

window.fetch = async (input, init) => {
  const request = input instanceof Request ? input : undefined;
  const url = new URL(input instanceof Request ? input.url : input, window.location.href);
  const method = (init?.method ?? request?.method ?? "GET").toUpperCase();
  const headers = new Headers(init?.headers ?? request?.headers);

  // Firefox aborts ordinary fetches before pagehide. Next interprets an aborted
  // RSC read as an MPA fallback, which can replace the user's new destination.
  // Keep these finite reads alive; explicit aborts and genuine failures propagate.
  if (url.origin === window.location.origin && method === "GET" && headers.get("rsc") === "1") {
    return originalFetch(input, { ...init, keepalive: true });
  }
  return originalFetch(input, init);
};
