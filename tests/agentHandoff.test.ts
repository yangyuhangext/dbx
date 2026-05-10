import { strict as assert } from "node:assert";
import test from "node:test";
import {
  deriveHandoffDialogState,
  mergeLoadedHandoffs,
  updateHandoffStatus,
  type AgentHandoffItem,
} from "../src/lib/agentHandoff.ts";

function handoff(id: string, status: AgentHandoffItem["status"]): AgentHandoffItem {
  return {
    id,
    createdAt: `2026-05-10T00:00:0${id}.000Z`,
    createdBy: "dbx-cli",
    connectionId: `conn-${id}`,
    connectionName: `Connection ${id}`,
    database: "main",
    title: `Review ${id}`,
    sql: "UPDATE users SET active = 0",
    operationClass: "write",
    riskLevel: "high",
    isProduction: true,
    status,
  };
}

test("mergeLoadedHandoffs keeps only queued and shown records in FIFO order", () => {
  const merged = mergeLoadedHandoffs([
    handoff("3", "rejected"),
    handoff("2", "shown"),
    handoff("1", "queued"),
    handoff("4", "executed"),
  ]);

  assert.deepEqual(
    merged.map((item) => [item.id, item.status]),
    [
      ["1", "queued"],
      ["2", "shown"],
    ],
  );
});

test("deriveHandoffDialogState opens the first pending handoff", () => {
  const state = deriveHandoffDialogState([handoff("1", "queued"), handoff("2", "shown")], null);

  assert.equal(state.open, true);
  assert.equal(state.active?.id, "1");
});

test("updateHandoffStatus marks shown and removes rejected handoffs from pending view", () => {
  const shown = updateHandoffStatus([handoff("1", "queued")], "1", "shown");
  assert.equal(shown[0].status, "shown");

  const rejected = updateHandoffStatus(shown, "1", "rejected");
  assert.deepEqual(rejected, []);
});
