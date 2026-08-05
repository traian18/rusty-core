/**
 * Minimal end-to-end example: spawn harnessd, create a session, send one
 * prompt, and print streamed text deltas until the run completes.
 *
 * Build the workspace first (`cargo build --release`), then run:
 *
 *   ANTHROPIC_API_KEY=sk-... node --experimental-strip-types examples/basic-chat.ts
 *
 * or compile with `npm run build` and run the emitted JS.
 */

import { HarnessClient } from "../src/client.js";
import { StdioSidecarTransport } from "../src/transport.js";

async function main(): Promise<void> {
  const transport = new StdioSidecarTransport({
    command: "../../../target/release/harnessd",
    onStderrLine: (line) => console.error(`[harnessd] ${line}`),
  });

  const client = await HarnessClient.connect(transport);

  const session = await client.createSession({
    workspaceRoot: process.cwd(),
    integration: "anthropic",
    integrationConfig: {},
  });

  await session.subscribe((event) => {
    const payload = event.event;
    if (typeof payload !== "string" && "AssistantTextDelta" in payload) {
      process.stdout.write(payload.AssistantTextDelta.delta);
    } else if (typeof payload !== "string" && "Completed" in payload) {
      process.stdout.write("\n");
    } else if (typeof payload !== "string" && "Failed" in payload) {
      console.error("run failed:", payload.Failed.error);
    }
  });

  await session.prompt("In one short sentence, what is a Rust trait?");

  await new Promise((resolve) => setTimeout(resolve, 15_000));

  await session.close();
  await client.close();
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
