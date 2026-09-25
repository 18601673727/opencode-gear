"use client";

import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { Activity, Clock3, Coins, Gauge, GitBranch, RefreshCw, Users } from "lucide-react";
import { useId, useMemo } from "react";
import { cn } from "@/lib/utils";
import {
  aggregateModelStats,
  aggregateProviderStats,
  type RuntimeObservability,
  type UsageValue,
  type WorkerRuntimeStats,
} from "../runtime/observability";

function formatTokens(value?: UsageValue): string {
  if (!value) return "—";
  const amount = value.value >= 1000 ? `${(value.value / 1000).toFixed(value.value >= 10000 ? 1 : 2)}K` : String(value.value);
  return value.provenance === "estimated" ? `≈ ${amount}` : amount;
}

function formatCost(value?: UsageValue): string {
  if (!value) return "—";
  const amount = `$${(value.value / 1_000_000).toFixed(3)}`;
  return value.provenance === "estimated" ? `≈ ${amount}` : amount;
}

function formatDuration(milliseconds?: number): string {
  if (milliseconds === undefined) return "—";
  if (milliseconds < 1000) return `${milliseconds}ms`;
  const seconds = milliseconds / 1000;
  return seconds < 60 ? `${seconds.toFixed(1)}s` : `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
}

function formatNumber(value?: number): string {
  return value === undefined ? "—" : new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 }).format(value);
}

function ProvenanceLabel({ value }: { value?: UsageValue }) {
  if (!value) return null;
  return (
    <span className="text-[10px] font-normal text-muted-foreground">
      {value.provenance === "estimated" ? "estimated" : "reported"}
    </span>
  );
}

function Metric({ label, value, icon: Icon }: { label: string; value: string; icon: typeof Activity }) {
  return (
    <div className="min-w-0 rounded-md border border-border bg-background px-2 py-1.5">
      <div className="flex items-center gap-1 text-[10px] text-muted-foreground">
        <Icon className="size-3" aria-hidden="true" />
        <span className="truncate">{label}</span>
      </div>
      <p className="mt-0.5 truncate text-[13px] font-semibold tabular-nums">{value}</p>
    </div>
  );
}

function TokenBreakdown({ usage: tokenUsage }: { usage: RuntimeObservability["mission"]["tokenUsage"] }) {
  return (
    <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4 lg:grid-cols-2">
      <Metric label="Total tokens" value={formatTokens(tokenUsage.total)} icon={Gauge} />
      <Metric label="Input" value={formatTokens(tokenUsage.input)} icon={Activity} />
      <Metric label="Output" value={formatTokens(tokenUsage.output)} icon={Activity} />
      <Metric label="Reasoning" value={formatTokens(tokenUsage.reasoning)} icon={GitBranch} />
      <Metric label="Cache read" value={formatTokens(tokenUsage.cacheRead)} icon={Activity} />
      <Metric label="Cache write" value={formatTokens(tokenUsage.cacheWrite)} icon={Activity} />
    </div>
  );
}

function WorkerRow({ worker }: { worker: WorkerRuntimeStats }) {
  const total = worker.tokenUsage.total;
  return (
    <li className="rounded-md border border-border px-2 py-2">
      <div className="flex items-start gap-2">
        <span
          className={cn(
            "mt-1 size-1.5 shrink-0 rounded-full",
            worker.status === "active" && "animate-pulse bg-amber-500",
            worker.status === "completed" && "bg-emerald-500",
            worker.status === "failed" && "bg-red-500",
            worker.status === "idle" && "bg-muted-foreground/40",
          )}
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1.5">
            <p className="truncate text-[12px] font-medium">{worker.label}</p>
            {worker.role === "lead" && <span className="rounded border border-border px-1 text-[9px] text-muted-foreground">lead</span>}
          </div>
          <p className="truncate text-[10px] text-muted-foreground" title={`${worker.provider} · ${worker.model}`}>
            {worker.provider} · {worker.model}{worker.variant ? ` · ${worker.variant}` : ""}
          </p>
        </div>
        <span className="shrink-0 text-[10px] capitalize text-muted-foreground">{worker.status}</span>
      </div>
      <div className="mt-1.5 grid grid-cols-2 gap-x-3 gap-y-1 pl-3.5 text-[10px] text-muted-foreground sm:grid-cols-4 lg:grid-cols-2">
        <span>tokens <strong className="font-medium text-foreground" title={total ? `${total.provenance} usage` : "Unavailable"}>{formatTokens(total)}</strong></span>
        <span>calls <strong className="font-medium text-foreground">{worker.invocationCount}</strong>{worker.retryCount ? ` · ${worker.retryCount} retry` : ""}</span>
        <span>elapsed <strong className="font-medium text-foreground">{formatDuration(worker.elapsedMs)}</strong></span>
        <span>latency <strong className="font-medium text-foreground">{formatDuration(worker.latencyMs)}</strong></span>
        <span>throughput <strong className="font-medium text-foreground">{worker.tokensPerSecond === undefined ? "—" : `${formatNumber(worker.tokensPerSecond)}/s`}</strong></span>
      </div>
    </li>
  );
}

function TimelineChart({ observability }: { observability: RuntimeObservability }) {
  const chartId = useId().replace(/:/g, "");
  const data = observability.timeline.map((point) => ({
    timestamp: point.timestamp,
    estimated: point.cumulativeUsage.total?.provenance === "estimated" ? point.cumulativeUsage.total.value : null,
    reported: point.cumulativeUsage.total?.provenance === "reported" ? point.cumulativeUsage.total.value : null,
  }));
  return (
    <div className="h-44 min-w-0 w-full">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 8, right: 4, left: -22, bottom: 0 }}>
          <defs>
            <linearGradient id={`${chartId}-estimated`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--color-chart-2)" stopOpacity={0.22} />
              <stop offset="100%" stopColor="var(--color-chart-2)" stopOpacity={0} />
            </linearGradient>
            <linearGradient id={`${chartId}-reported`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--color-chart-1)" stopOpacity={0.22} />
              <stop offset="100%" stopColor="var(--color-chart-1)" stopOpacity={0} />
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" stroke="var(--color-border)" vertical={false} />
          <XAxis dataKey="timestamp" tick={{ fontSize: 9, fill: "var(--color-muted-foreground)" }} axisLine={false} tickLine={false} />
          <YAxis tick={{ fontSize: 9, fill: "var(--color-muted-foreground)" }} axisLine={false} tickLine={false} width={32} />
          <Tooltip formatter={(value, name) => [`${value ?? "—"} tokens`, name === "estimated" ? "Estimated" : "Reported"]} />
          <Area type="monotone" dataKey="estimated" stroke="var(--color-chart-2)" fill={`url(#${chartId}-estimated)`} strokeDasharray="4 3" connectNulls={false} isAnimationActive={false} />
          <Area type="monotone" dataKey="reported" stroke="var(--color-chart-1)" fill={`url(#${chartId}-reported)`} connectNulls={false} isAnimationActive={false} />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

function WorkerBreakdown({ workers }: { workers: WorkerRuntimeStats[] }) {
  const data = workers
    .map((worker) => ({ name: worker.label, tokens: worker.tokenUsage.total?.value }))
    .filter((worker): worker is { name: string; tokens: number } => worker.tokens !== undefined);
  if (data.length === 0) return <p className="text-[11px] text-muted-foreground">Token totals are unavailable.</p>;
  return (
    <div className="h-44 min-w-0 w-full">
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={data} layout="vertical" margin={{ top: 2, right: 8, left: 4, bottom: 2 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="var(--color-border)" horizontal={false} />
          <XAxis type="number" tick={{ fontSize: 9, fill: "var(--color-muted-foreground)" }} axisLine={false} tickLine={false} />
          <YAxis type="category" dataKey="name" width={70} tick={{ fontSize: 9, fill: "var(--color-muted-foreground)" }} axisLine={false} tickLine={false} />
          <Tooltip formatter={(value) => [`${value ?? "—"} tokens`, "Total"]} />
          <Bar dataKey="tokens" fill="var(--color-chart-1)" radius={[0, 3, 3, 0]} barSize={12} isAnimationActive={false} />
        </BarChart>
      </ResponsiveContainer>
    </div>
  );
}

function AggregateRow({ name, detail, tokens, calls, active, cost, success, failure, latency }: { name: string; detail?: string; tokens?: UsageValue; calls: number; active: number; cost?: UsageValue; success?: number; failure?: number; latency?: number }) {
  const outcome = success === undefined && failure === undefined ? undefined : `${success ?? "—"} ok · ${failure ?? "—"} failed`;
  const aggregateDetail = [detail, outcome, latency === undefined ? undefined : `${formatDuration(latency)} latency`].filter(Boolean).join(" · ");
  return (
    <li className="flex items-center gap-2 border-b border-border/70 py-1.5 last:border-0">
      <div className="min-w-0 flex-1">
        <p className="truncate text-[11px] font-medium">{name}</p>
        {aggregateDetail && <p className="truncate text-[10px] text-muted-foreground">{aggregateDetail}</p>}
      </div>
      <span className="shrink-0 text-right text-[10px] text-muted-foreground" title={tokens ? `${tokens.provenance} usage` : "Unavailable"}>{formatTokens(tokens)} · {calls} calls</span>
      <span className="w-12 shrink-0 text-right text-[10px] text-muted-foreground">{active} active</span>
      <span className="w-12 shrink-0 text-right text-[10px] text-muted-foreground">{formatCost(cost)}</span>
    </li>
  );
}

export function ObservabilityPanel({ observability }: { observability: RuntimeObservability }) {
  const providers = useMemo(() => aggregateProviderStats(observability.workers), [observability.workers]);
  const models = useMemo(() => aggregateModelStats(observability.workers), [observability.workers]);
  const mission = observability.mission;
  return (
    <section className="mt-4 border-t border-border pt-3" aria-label="Runtime observability">
      <div className="flex items-center gap-1.5">
        <Activity className="size-3.5 text-muted-foreground" aria-hidden="true" />
        <h3 className="text-[11px] font-semibold tracking-wider text-muted-foreground uppercase">Runtime observability</h3>
        <span className="ml-auto text-[10px] text-muted-foreground" title="Token provenance is preserved in the runtime model">estimated / reported</span>
      </div>

      <div className="mt-2 grid grid-cols-2 gap-1.5">
        <Metric label="Mission tokens" value={formatTokens(mission.tokenUsage.total)} icon={Gauge} />
        <Metric label="Cost" value={formatCost(mission.costMicros)} icon={Coins} />
        <Metric label="Elapsed" value={formatDuration(mission.elapsedMs)} icon={Clock3} />
        <Metric label="Workers active" value={`${mission.activeWorkerCount} · ${mission.invocationCount} calls`} icon={Users} />
      </div>
      <div className="mt-1 text-right text-[10px] text-muted-foreground">
        <ProvenanceLabel value={mission.tokenUsage.total} />
      </div>
      <TokenBreakdown usage={mission.tokenUsage} />

      <details className="group mt-3" open>
        <summary className="cursor-pointer list-none text-[11px] font-semibold tracking-wider text-muted-foreground uppercase marker:hidden">
          <span className="group-open:text-foreground">Runtime detail</span>
          <span className="ml-1 text-[10px] font-normal normal-case">(workers, timeline, resources)</span>
        </summary>
        <div className="mt-2 flex flex-col gap-3">
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <h4 className="text-[11px] font-medium">Participants</h4>
              <span className="text-[10px] text-muted-foreground">{observability.workers.length} total</span>
            </div>
            <ul className="flex flex-col gap-1.5">{observability.workers.map((worker) => <WorkerRow key={worker.workerId} worker={worker} />)}</ul>
          </div>

          <div>
            <div className="mb-1.5 flex items-center gap-1.5">
              <h4 className="text-[11px] font-medium">Mission token timeline</h4>
              <span className="text-[10px] text-muted-foreground">cumulative</span>
            </div>
            <TimelineChart observability={observability} />
            <div className="flex justify-end gap-3 text-[10px] text-muted-foreground"><span>— estimated</span><span>— reported</span></div>
          </div>

          <div>
            <div className="mb-1.5 flex items-center gap-1.5"><h4 className="text-[11px] font-medium">Worker resource breakdown</h4><RefreshCw className="size-3 text-muted-foreground" aria-hidden="true" /></div>
            <WorkerBreakdown workers={observability.workers} />
          </div>

          <div>
            <h4 className="mb-1 text-[11px] font-medium">Provider stats</h4>
            <ul className="rounded-md border border-border px-2">
              {providers.map((provider) => <AggregateRow key={provider.provider} name={provider.provider} tokens={provider.tokenUsage.total} calls={provider.invocationCount} active={provider.activeWorkers} cost={provider.costMicros} success={provider.successCount} failure={provider.failureCount} latency={provider.latencyMs} />)}
            </ul>
          </div>

          <div>
            <h4 className="mb-1 text-[11px] font-medium">Model stats</h4>
            <ul className="rounded-md border border-border px-2">
              {models.map((model) => <AggregateRow key={`${model.provider}-${model.model}`} name={model.model} detail={model.provider} tokens={model.tokenUsage.total} calls={model.invocationCount} active={model.activeWorkers} cost={model.costMicros} success={model.successCount} failure={model.failureCount} latency={model.latencyMs} />)}
            </ul>
          </div>
        </div>
      </details>
    </section>
  );
}
