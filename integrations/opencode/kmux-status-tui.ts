/**
 * kmux status tracking for the OpenCode TUI.
 *
 * The server-side plugin can run outside tmux when OpenCode is daemonized. This
 * TUI plugin runs in the visible `opencode` process, so tmux can identify the
 * current pane and update the window status option reliably.
 */

import type { TuiPlugin } from "@opencode-ai/plugin/tui";

import { waitForKmuxChild } from "./kmux-child-process";
import { KmuxCommandQueue } from "./kmux-command-queue";

type KmuxStatus = "working" | "waiting" | "done" | "clear";
type TuiApi = Parameters<TuiPlugin>[0];
type Message = ReturnType<TuiApi["state"]["session"]["messages"]>[number];
type AssistantMessage = Extract<Message, { role: "assistant" }>;
type SpawnKmux = (cmd: string[]) => void;
type TuiReporterState = {
  lastReportKey?: string;
  lastRootSessionID?: string;
  sessionStatus: Map<string, KmuxStatus>;
  reportedProducerInstances: Map<string, string>;
};

declare const Bun: {
  env: Record<string, string | undefined>;
  spawn(input: { cmd: string[]; stdout?: "ignore"; stderr?: "ignore" }): {
    exited: Promise<number>;
    kill(signal?: number): void;
  };
};

const AGENT_KIND = "opencode";
const PRODUCER_KIND = "tui";
const COMMAND_TIMEOUT_MS = 2000;

const forwardedEventTypes = [
  "session.status",
  "session.idle",
  "session.error",
  "permission.asked",
  "permission.replied",
  "question.asked",
  "question.replied",
  "question.rejected",
] as const;

const metadataEventTypes = ["session.updated", "message.updated", "message.removed"] as const;

function clean(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed || undefined;
}

function envValue(name: string): string | undefined {
  return clean(Bun.env[name]);
}

function producerInstance(): string {
  const tmuxInstance =
    envValue("KMUX_TMUX_SOCKET_NAME") ?? envValue("TMUX")?.split(",")[0] ?? "default";
  return `${tmuxInstance}/${envValue("TMUX_PANE") ?? "no-pane"}`;
}

function pushArg(cmd: string[], flag: string, value: string | undefined) {
  if (value) cmd.push(flag, value);
}

function runKmux(cmd: ReadonlyArray<string>): Promise<number> {
  const child = Bun.spawn({
    cmd: [...cmd],
    stdout: "ignore",
    stderr: "ignore",
  });
  return waitForKmuxChild(child, COMMAND_TIMEOUT_MS);
}

function clearSessionReport(state: TuiReporterState, spawnKmux: SpawnKmux, sessionID: string) {
  const instance = state.reportedProducerInstances.get(sessionID) ?? producerInstance();
  state.reportedProducerInstances.delete(sessionID);
  spawnKmux([
    "kmux",
    "set-agent-status",
    "--agent-kind",
    AGENT_KIND,
    "--session-id",
    sessionID,
    "--producer-kind",
    PRODUCER_KIND,
    "--producer-instance",
    instance,
    "--delete",
  ]);
}

function statusFromEvent(event: { type: string; properties?: unknown }): KmuxStatus | undefined {
  switch (event.type) {
    case "session.status": {
      const properties =
        typeof event.properties === "object" && event.properties !== null
          ? (event.properties as { status?: unknown })
          : undefined;
      const status = properties?.status;
      const statusType =
        typeof status === "object" && status !== null && "type" in status
          ? (status as { type?: unknown }).type
          : status;

      if (statusType === "busy" || statusType === "retry") return "working";
      if (statusType === "idle") return "done";
      return undefined;
    }
    case "permission.asked":
    case "question.asked":
      return "waiting";
    case "permission.replied":
    case "question.replied":
    case "question.rejected":
      return "working";
    case "session.idle":
    case "session.error":
      return "done";
    default:
      return undefined;
  }
}

function eventSessionID(event: { properties?: unknown }): string | undefined {
  const properties =
    typeof event.properties === "object" && event.properties !== null
      ? (event.properties as { sessionID?: unknown })
      : undefined;
  if (typeof properties?.sessionID !== "string") return undefined;
  return properties.sessionID;
}

function eventMatchesActiveSession(
  state: TuiReporterState,
  api: TuiApi,
  event: { properties?: unknown },
): boolean {
  const rootSessionID = activeSessionID(state, api);
  const sessionID = eventSessionID(event);
  if (!rootSessionID || !sessionID) return false;
  if (sessionID === rootSessionID) return true;

  const session = api.state.session.get(sessionID);
  if (session?.parentID === rootSessionID) return true;

  const properties =
    typeof event.properties === "object" && event.properties !== null
      ? (event.properties as { info?: unknown })
      : undefined;
  const info =
    typeof properties?.info === "object" && properties.info !== null
      ? (properties.info as { parentID?: unknown })
      : undefined;
  return info?.parentID === rootSessionID;
}

function recordEventStatus(
  state: TuiReporterState,
  event: { properties?: unknown },
  status: KmuxStatus,
) {
  const sessionID = eventSessionID(event);
  if (sessionID) state.sessionStatus.set(sessionID, status);
}

function statusPriority(status: KmuxStatus): number {
  switch (status) {
    case "waiting":
      return 3;
    case "working":
      return 2;
    case "done":
      return 1;
    case "clear":
      return 0;
  }
}

function mostImportantStatus(statuses: KmuxStatus[]): KmuxStatus {
  let best: KmuxStatus = "clear";
  for (const status of statuses) {
    if (statusPriority(status) > statusPriority(best)) best = status;
  }
  return best;
}

function sessionBelongsToRoot(api: TuiApi, sessionID: string, rootSessionID: string) {
  if (sessionID === rootSessionID) return true;
  return api.state.session.get(sessionID)?.parentID === rootSessionID;
}

function cachedFamilySessionIDs(
  state: TuiReporterState,
  api: TuiApi,
  rootSessionID: string,
): string[] {
  return Array.from(state.sessionStatus.keys()).filter((sessionID) =>
    sessionBelongsToRoot(api, sessionID, rootSessionID),
  );
}

function formatNumber(num: number): string {
  if (num >= 1000000) return `${(num / 1000000).toFixed(1)}M`;
  if (num >= 1000) return `${(num / 1000).toFixed(1)}K`;
  return num.toString();
}

function isAssistantWithOutputTokens(message: Message): message is AssistantMessage {
  return message.role === "assistant" && message.tokens.output > 0;
}

function lastAssistantWithOutputTokens(
  messages: ReadonlyArray<Message>,
): AssistantMessage | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message && isAssistantWithOutputTokens(message)) return message;
  }
}

function sessionTitle(state: TuiReporterState, api: TuiApi): string | undefined {
  const sessionID = activeSessionID(state, api);
  if (!sessionID) return undefined;

  const session = api.state.session.get(sessionID);
  const title = session?.title?.trim();
  if (title) return title;

  const slug = session?.slug?.trim();
  return slug || undefined;
}

function contextUsage(state: TuiReporterState, api: TuiApi): string | undefined {
  const sessionID = activeSessionID(state, api);
  if (!sessionID) return undefined;

  const messages = api.state.session.messages(sessionID);
  const last = lastAssistantWithOutputTokens(messages);
  if (!last) return undefined;

  const tokens =
    last.tokens.input +
    last.tokens.output +
    last.tokens.reasoning +
    last.tokens.cache.read +
    last.tokens.cache.write;
  if (tokens <= 0) return undefined;

  const model = api.state.provider.find((item) => item.id === last.providerID)?.models[
    last.modelID
  ];
  const pct = model?.limit.context
    ? `${Math.round((tokens / model.limit.context) * 100)}%`
    : undefined;
  return pct ? `${formatNumber(tokens)} (${pct})` : formatNumber(tokens);
}

function activeSessionDirectory(api: TuiApi, sessionID: string): string | undefined {
  const session = api.state.session.get(sessionID);
  return clean(session?.directory) ?? envValue("PWD");
}

function activeSessionWorkspaceID(api: TuiApi, sessionID: string): string | undefined {
  return clean(api.state.session.get(sessionID)?.workspaceID);
}

function reportStatus(
  state: TuiReporterState,
  spawnKmux: SpawnKmux,
  api: TuiApi,
  status: KmuxStatus,
) {
  const sessionID = activeSessionID(state, api) ?? state.lastRootSessionID;
  if (!sessionID) return;

  const deleting = status === "clear";
  const title = deleting ? undefined : sessionTitle(state, api);
  const context = deleting ? undefined : contextUsage(state, api);
  const paneID = envValue("TMUX_PANE");
  const tmuxInstance = envValue("KMUX_TMUX_SOCKET_NAME");
  const directory = deleting ? undefined : activeSessionDirectory(api, sessionID);
  const workspaceID = deleting ? undefined : activeSessionWorkspaceID(api, sessionID);
  const instance = deleting
    ? (state.reportedProducerInstances.get(sessionID) ?? producerInstance())
    : producerInstance();
  const reportKey = JSON.stringify({
    status,
    sessionID,
    title,
    context,
    paneID,
    tmuxInstance,
    directory,
    workspaceID,
    instance,
  });
  if (state.lastReportKey === reportKey) return;
  state.lastReportKey = reportKey;

  const cmd = ["kmux", "set-agent-status"];
  if (!deleting) cmd.push(status);
  cmd.push(
    "--agent-kind",
    AGENT_KIND,
    "--session-id",
    sessionID,
    "--producer-kind",
    PRODUCER_KIND,
    "--producer-instance",
    instance,
  );
  if (deleting) {
    clearSessionReport(state, spawnKmux, sessionID);
    return;
  } else {
    state.reportedProducerInstances.set(sessionID, instance);
    pushArg(cmd, "--title", title);
    pushArg(cmd, "--context", context);
    pushArg(cmd, "--tmux-instance", tmuxInstance);
    if (workspaceID) cmd.push("--agent-meta", `workspace_id=${workspaceID}`);
    else cmd.push("--clear-agent-meta", "workspace_id");
    pushArg(cmd, "--directory", directory);
  }

  spawnKmux(cmd);
}

function activeRouteSessionID(api: TuiApi) {
  const current = api.route.current;
  const sessionID = current.name === "session" ? current.params?.sessionID : undefined;
  return typeof sessionID === "string" ? sessionID : undefined;
}

function activeSessionID(state: TuiReporterState, api: TuiApi) {
  const sessionID = activeRouteSessionID(api);
  if (!sessionID) return state.lastRootSessionID;

  // Subagent routes have their own session IDs, but kmux should track the parent row.
  const session = api.state.session.get(sessionID);
  const rootSessionID = session?.parentID ?? sessionID;
  state.lastRootSessionID = rootSessionID;
  return rootSessionID;
}

function statusFromCurrentSession(state: TuiReporterState, api: TuiApi): KmuxStatus {
  if (!api.state.ready) return "clear";

  const rootSessionID = activeSessionID(state, api);
  if (!rootSessionID) return "clear";

  const routeSessionID = activeRouteSessionID(api);
  const sessionIDs = new Set([...cachedFamilySessionIDs(state, api, rootSessionID), rootSessionID]);
  if (routeSessionID && routeSessionID !== rootSessionID) {
    sessionIDs.add(routeSessionID);
  }
  const statuses: KmuxStatus[] = [];

  for (const sessionID of sessionIDs.values()) {
    if (api.state.session.permission(sessionID).length > 0) {
      statuses.push("waiting");
      continue;
    }
    if (api.state.session.question(sessionID).length > 0) {
      statuses.push("waiting");
      continue;
    }

    const status = api.state.session.status(sessionID);
    switch (status?.type) {
      case "busy":
      case "retry":
        statuses.push("working");
        break;
      default:
        statuses.push("done");
        break;
    }
  }

  return mostImportantStatus(statuses);
}

const tui: TuiPlugin = async (api) => {
  const state: TuiReporterState = {
    sessionStatus: new Map(),
    reportedProducerInstances: new Map(),
  };
  const kmuxCommands = new KmuxCommandQueue(runKmux);
  const spawnKmux: SpawnKmux = (cmd) => {
    // Keep OpenCode usable if kmux is unavailable. Failed or non-zero commands
    // remain best-effort, while later changed reports continue through the queue.
    void kmuxCommands.enqueue(cmd);
  };
  const disposers = forwardedEventTypes.map((type) =>
    api.event.on(type, (event) => {
      if (!eventMatchesActiveSession(state, api, event)) return;
      const status = statusFromEvent(event);
      if (status) {
        recordEventStatus(state, event, status);
        reportStatus(state, spawnKmux, api, statusFromCurrentSession(state, api));
      }
    }),
  );
  disposers.push(
    ...metadataEventTypes.map((type) =>
      api.event.on(type, (event) => {
        if (!eventMatchesActiveSession(state, api, event)) return;
        reportStatus(state, spawnKmux, api, statusFromCurrentSession(state, api));
      }),
    ),
  );

  reportStatus(state, spawnKmux, api, statusFromCurrentSession(state, api));

  const timer = setInterval(() => {
    reportStatus(state, spawnKmux, api, statusFromCurrentSession(state, api));
  }, 300);

  api.lifecycle.onDispose(async () => {
    clearInterval(timer);
    for (const dispose of disposers) dispose();
    await kmuxCommands.drain();
  });
};

const plugin: { id: string; tui: TuiPlugin } = {
  id: "kmux-status",
  tui,
};

export default plugin;
