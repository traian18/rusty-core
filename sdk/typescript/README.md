# @rusty/harness-sdk

TypeScript client for harnessd protocol v2.

The SDK performs the required hello handshake and exposes:

- session create, list, strict restore, snapshot, and close;
- prompt, steer, follow-up, cancel, and permission mutations;
- client command IDs, optional expected revisions, and typed admission receipts;
- structured RPC errors with stable codes, categories, retryability, and correlation fields;
- resumable event subscription and explicit event-gap callbacks;
- exact Rust wire shapes for toolsets and attachment byte arrays.

`HarnessClient.capabilities.durable_idempotency` is currently false. Reusing
a command ID protects ambiguous retries while the daemon remains alive, but
admission history is not yet restored after a daemon restart.

The Rust DTOs are authoritative. The language-neutral mirror is
[`protocol-v2.schema.json`](../../schema/protocol-v2.schema.json).

## Development

```sh
npm ci
npm run build
npm test
```
