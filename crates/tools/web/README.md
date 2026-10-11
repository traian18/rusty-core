# harness-tool-web

Web access for agents.

## What's inside

- `web_fetch` — fetch a URL and return its content as text, with a built-in SSRF guard that blocks requests to private and local addresses.

Web search is deferred: it needs a search-provider decision that `web_fetch` does not.

## In the workspace

- **Depends on:** `harness-tools`
- **Used by:** `harness-engine`
