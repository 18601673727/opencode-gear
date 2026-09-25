import assert from "node:assert/strict";
import { test } from "node:test";
import {
  aggregateModelStats,
  aggregateProviderStats,
  boundTimeline,
  sumTokenUsage,
  sumUsageValues,
  type WorkerRuntimeStats,
} from "./observability";
import { createScenarioFixture } from "./scenarios";

function worker(overrides: Partial<WorkerRuntimeStats> = {}): WorkerRuntimeStats {
  return {
    workerId: "worker-1",
    role: "worker",
    label: "Worker",
    provider: "Provider A",
    model: "Model A",
    status: "completed",
    invocationCount: 1,
    retryCount: 0,
    successCount: 1,
    tokenUsage: { input: { value: 100, provenance: "reported" }, total: { value: 150, provenance: "reported" } },
    ...overrides,
  };
}

test("aggregates workers by provider and model without losing provenance", () => {
  const workers = [
    worker(),
    worker({ workerId: "worker-2", provider: "Provider A", model: "Model B", status: "active", invocationCount: 2, tokenUsage: { input: { value: 50, provenance: "estimated" }, total: { value: 80, provenance: "estimated" } } }),
    worker({ workerId: "worker-3", provider: "Provider B", model: "Model C", tokenUsage: { total: { value: 40, provenance: "reported" } } }),
  ];

  const providers = aggregateProviderStats(workers);
  assert.equal(providers.length, 2);
  assert.equal(providers[0].invocationCount, 3);
  assert.equal(providers[0].activeWorkers, 1);
  assert.deepEqual(providers[0].tokenUsage.total, { value: 230, provenance: "estimated" });

  const models = aggregateModelStats(workers);
  assert.equal(models.length, 3);
  assert.equal(models.find((model) => model.model === "Model B")?.activeWorkers, 1);
});

test("optional provider metrics remain unavailable instead of becoming zero", () => {
  const combined = sumTokenUsage([{ input: { value: 12, provenance: "reported" } }, { output: { value: 4, provenance: "reported" } }]);
  assert.equal(combined.reasoning, undefined);
  assert.equal(combined.cacheRead, undefined);
  assert.equal(combined.total, undefined);
  assert.equal(sumUsageValues([undefined, undefined]), undefined);
  assert.equal(aggregateProviderStats([worker()])[0].ttftMs, undefined);
});

test("estimated usage transitions to reported usage explicitly", () => {
  assert.deepEqual(sumUsageValues([
    { value: 24000, provenance: "estimated" },
    { value: 25300, provenance: "reported" },
  ]), { value: 49300, provenance: "estimated" });
  assert.deepEqual(sumUsageValues([{ value: 25300, provenance: "reported" }]), { value: 25300, provenance: "reported" });
});

test("timeline history is bounded to the newest points", () => {
  const points = Array.from({ length: 65 }, (_, index) => ({ timestamp: String(index), elapsedMs: index, cumulativeUsage: {} }));
  const bounded = boundTimeline(points, 60);
  assert.equal(bounded.length, 60);
  assert.equal(bounded[0].timestamp, "5");
});

test("observability live fixture is deterministic and includes staged updates", () => {
  const first = createScenarioFixture("observability-live");
  const second = createScenarioFixture("observability-live");
  assert.deepEqual(first.observabilityBySession, second.observabilityBySession);
  assert.deepEqual(first.observabilityUpdates, second.observabilityUpdates);
  assert.equal(first.observabilityUpdates?.length, 3);
  assert.equal(first.observabilityBySession["design-pwa-shell"]?.workers.some((item) => item.role === "lead"), true);
});
