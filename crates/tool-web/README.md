# rusty-tool-web

Web access for agents: `web_fetch` fetches a URL and returns its content as text, behind a
guard that refuses requests to private and local addresses. Web search is deferred: it needs a
search-provider decision that fetching does not.

## Why it exists

Fetching a page is simple; fetching it safely is not. A model that can be told to open any URL
can be steered into reading cloud metadata endpoints or internal services (server-side request
forgery). This crate makes the safe behavior the only behavior.

## How it works

`FetchTool::execute` validates the URL, resolves its host (`ssrf.rs`), and rejects any address that
is private, loopback, link-local or otherwise internal. The HTTP client is configured **not** to
follow redirects: a redirect response is returned as a result that names the `Location`, and the
model can fetch it explicitly, at which point the guard checks that address too. The body is
read with a size cap (`read_capped`) and returned as text, with a notice when it was cut.

## Key types and concepts

- **`FetchTool`, `FetchInput { url }`**: the executor and its argument.
- **SSRF guard** (`ssrf`): checks the resolved address of every URL that is fetched.
- **Size cap**: a page larger than the limit is truncated rather than loaded whole.

## A worked example

```json
{ "tool": "web_fetch", "arguments": { "url": "https://doc.rust-lang.org/book/" } }
```

`http://169.254.169.254/latest/meta-data/` and `http://localhost:8080/` are refused with an error
result naming the reason. A public URL that redirects is not followed automatically: the result
says "redirected to <location>; not followed automatically", and fetching that location is a new,
separately checked call.

## Public API

- `FetchTool`, `FetchInput`.

## Errors

A blocked address, an unsupported scheme, a timeout, a non-success status or a too-large
response is an error result with a clear message. Building the HTTP client at construction is the one place that can panic, and
it states why: the client configuration is fixed and always builds.

## Design decisions

- **Redirects are never followed automatically.** An automatic redirect could land on a blocked
  address after the first check passed.
- **Check after resolution.** Checking the hostname alone would miss a name that resolves to a
  private address.

## Testing

Unit tests in `src/ssrf.rs` cover blocked address ranges, and `src/fetch/tests.rs` covers
fetching, redirects and the size cap. Run `cargo test -p rusty-tool-web`.

## In the workspace

- **Depends on:** `rusty-tool-api`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
- Fetch an address in a private, loopback or link-local range, directly or by redirect.
