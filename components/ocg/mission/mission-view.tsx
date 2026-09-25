"use client";

import {
  Check,
  Circle,
  CircleDot,
  Clock,
  Cpu,
  AlertCircle,
  Loader2,
  Wallet,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ActivityPulse } from "../activity-pulse";
import type { Mission } from "../types";

type MissionViewProps = {
  mission: Mission;
  onClose: () => void;
};

function TaskIcon({ status }: { status: Mission["tasks"][number]["status"] }) {
  if (status === "completed")
    return <Check className="size-3.5 text-emerald-600 dark:text-emerald-400" aria-hidden="true" />;
  if (status === "active")
    return <Loader2 className="size-3.5 animate-spin text-foreground" aria-hidden="true" />;
  if (status === "failed") return <AlertCircle className="size-3.5 text-red-500" aria-hidden="true" />;
  return <Circle className="size-3.5 text-muted-foreground/60" aria-hidden="true" />;
}

export function MissionView({ mission, onClose }: MissionViewProps) {
  const pct = Math.round((mission.completed / mission.total) * 100);
  return (
    <div className="flex h-full w-full flex-col">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2.5">
        <CircleDot className="size-4 text-muted-foreground" aria-hidden="true" />
        <h2 className="flex-1 text-[13px] font-semibold tracking-tight">Mission</h2>
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onClose}
          aria-label="Collapse mission panel"
          title="Collapse mission panel"
        >
          <X className="size-4" />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <p className="text-[14px] font-semibold tracking-tight">{mission.title}</p>
        <div className="mt-1.5 flex items-center gap-1.5">
          <span className="inline-flex items-center gap-1.5 rounded-full border border-border bg-muted/50 px-2 py-0.5 text-[11px] font-medium">
            <span
              className={cn(
                "size-1.5 rounded-full",
                mission.status === "running" && "animate-pulse bg-amber-500",
                mission.status === "completed" && "bg-emerald-500",
                mission.status === "failed" && "bg-red-500",
                mission.status === "budget-exhausted" && "bg-red-500",
                mission.status === "paused" && "bg-muted-foreground",
                mission.status === "planning" && "bg-sky-500",
              )}
              aria-hidden="true"
            />
            {mission.status}
          </span>
          <span className="text-[11px] text-muted-foreground">
            {mission.completed} / {mission.total} tasks
          </span>
        </div>

        <div
          className="mt-2.5"
          role="progressbar"
          aria-valuenow={mission.completed}
          aria-valuemin={0}
          aria-valuemax={mission.total}
          aria-label="Mission progress"
        >
          <div className="h-1.5 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-foreground transition-[width] duration-300 ease-out"
              style={{ width: `${pct}%` }}
            />
          </div>
          <p className="mt-1 text-[11px] text-muted-foreground">{pct}% complete · local fixture</p>
        </div>

        <p className="mt-3 text-[12px] leading-5 text-muted-foreground">{mission.goal}</p>

        <div className="mt-3 rounded-md border border-border bg-muted/30 px-2.5 py-2">
          <p className="text-[11px] font-semibold tracking-wider text-muted-foreground uppercase">
            Current
          </p>
          <p className="mt-0.5 text-[13px] font-medium">{mission.current}</p>
        </div>

        <h3 className="mt-4 mb-1 text-[11px] font-semibold tracking-wider text-muted-foreground uppercase">
          Tasks
        </h3>
        <ul className="flex flex-col">
          {mission.tasks.map((task) => (
            <li
              key={task.id}
              className={cn(
                "flex items-center gap-2 rounded-md px-2 py-1.5 text-[13px]",
                task.status === "active" && "bg-muted font-medium",
                 task.status === "completed" && "text-muted-foreground",
                 task.status === "pending" && "text-muted-foreground",
                 task.status === "failed" && "text-red-600 dark:text-red-400",
              )}
            >
              <TaskIcon status={task.status} />
              <span
                className={cn(
                  "flex-1 truncate",
                   task.status === "completed" && "line-through decoration-muted-foreground/50",
                )}
              >
                {task.title}
              </span>
              {task.status === "active" && (
                <span className="rounded border border-border bg-background px-1 text-[10px] text-muted-foreground">
                  now
                </span>
              )}
            </li>
          ))}
        </ul>

        <h3 className="mt-4 mb-1.5 text-[11px] font-semibold tracking-wider text-muted-foreground uppercase">
          Resources
        </h3>
        <dl className="flex flex-col gap-1.5 text-[12px]">
          <div className="flex items-center gap-2 rounded-md border border-border px-2.5 py-2">
            <Cpu className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
            <dt className="text-muted-foreground">Commitment</dt>
            <dd className="ml-auto font-medium">
              {mission.commitment.workers} workers · {mission.commitment.mode}
            </dd>
          </div>
          <div className="flex items-center gap-2 rounded-md border border-border px-2.5 py-2">
            <Wallet className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
            <dt className="text-muted-foreground">Budget</dt>
            <dd className={cn("ml-auto font-medium", mission.budget.status === "exhausted" && "text-red-600 dark:text-red-400")}>
              ${mission.budget.spent} / ${mission.budget.limit}
            </dd>
          </div>
          {mission.workers.map((worker) => (
            <div key={worker.id} className="flex items-center gap-2 rounded-md border border-border px-2.5 py-2">
              <ActivityPulse className="size-5 shrink-0" label={`${worker.name} activity`} />
              <dt className="text-muted-foreground">Worker</dt>
              <dd className="ml-auto text-right font-medium">
                {worker.name}
                <span className="block text-[11px] font-normal text-muted-foreground">
                  {worker.status}{worker.task ? ` · ${worker.task}` : ""}
                </span>
              </dd>
            </div>
          ))}
          <div className="flex items-center gap-2 rounded-md border border-border px-2.5 py-2">
            <Clock className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
            <dt className="text-muted-foreground">Elapsed</dt>
            <dd className="ml-auto font-medium">{mission.elapsed}</dd>
          </div>
        </dl>

        {mission.warnings.length > 0 && (
          <div className="mt-3 flex flex-col gap-1 rounded-md border border-amber-500/30 bg-amber-500/5 px-2.5 py-2 text-[11px] text-muted-foreground">
            {mission.warnings.map((warning) => <p key={warning}>{warning}</p>)}
          </div>
        )}

        <p className="mt-3 rounded-md bg-muted/40 px-2.5 py-2 text-[11px] leading-5 text-muted-foreground">
          Illustrative surface only — durable Mission state, persistence, and
          orchestration land in a later phase.
        </p>
      </div>
    </div>
  );
}
