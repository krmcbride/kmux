/**
 * kmux status tracking for the OpenCode TUI.
 *
 * The server-side plugin can run outside tmux when OpenCode is daemonized. This
 * TUI plugin runs in the visible `opencode` process, so tmux can identify the
 * current pane and update the window status option reliably.
 */

import type { TuiPlugin } from "@opencode-ai/plugin/tui";

type KmuxStatus = "working" | "waiting" | "done" | "clear";
type TuiApi = Parameters<TuiPlugin>[0];
type Message = ReturnType<TuiApi["state"]["session"]["messages"]>[number];
type AssistantMessage = Extract<Message, { role: "assistant" }>;

declare const Bun: {
  env: Record<string, string | undefined>;
  spawn(input: { cmd: string[]; stdout?: "ignore"; stderr?: "ignore" }): {
    exited: Promise<number>;
  };
};

const AGENT_KIND = "opencode";
const PRODUCER_KIND = "tui";

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

const metadataEventTypes = [
  "session.updated",
  "message.updated",
  "message.removed",
] as const;

let lastReportKey: string | undefined;
let lastRootSessionID: string | undefined;
let currentReportedSessionID: string | undefined;
const sessionStatus = new Map<string, KmuxStatus>();
const reportedProducerInstances = new Map<string, string>();

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
    envValue("KMUX_TMUX_SOCKET_NAME") ??
    envValue("TMUX")?.split(",")[0] ??
    "default";
  return `${tmuxInstance}/${envValue("TMUX_PANE") ?? "no-pane"}`;
}

function pushArg(cmd: string[], flag: string, value: string | undefined) {
  if (value) cmd.push(flag, value);
}

function spawnKmux(cmd: string[]) {
  try {
    void Bun.spawn({
      cmd,
      stdout: "ignore",
      stderr: "ignore",
    }).exited;
  } catch {
    // Keep OpenCode usable if kmux is unavailable or this TUI is outside tmux.
  }
}

function clearSessionReport(sessionID: string) {
  const instance =
    reportedProducerInstances.get(sessionID) ?? producerInstance();
  reportedProducerInstances.delete(sessionID);
  if (currentReportedSessionID === sessionID)
    currentReportedSessionID = undefined;
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

function statusFromEvent(event: {
  type: string;
  properties?: unknown;
}): KmuxStatus | undefined {
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
  api: TuiApi,
  event: { properties?: unknown },
): boolean {
  const rootSessionID = activeSessionID(api);
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
  event: { properties?: unknown },
  status: KmuxStatus,
) {
  const sessionID = eventSessionID(event);
  if (sessionID) sessionStatus.set(sessionID, status);
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

function sessionBelongsToRoot(
  api: TuiApi,
  sessionID: string,
  rootSessionID: string,
) {
  if (sessionID === rootSessionID) return true;
  return api.state.session.get(sessionID)?.parentID === rootSessionID;
}

function cachedFamilySessionIDs(api: TuiApi, rootSessionID: string): string[] {
  return Array.from(sessionStatus.keys()).filter((sessionID) =>
    sessionBelongsToRoot(api, sessionID, rootSessionID),
  );
}

function formatNumber(num: number): string {
  if (num >= 1000000) return `${(num / 1000000).toFixed(1)}M`;
  if (num >= 1000) return `${(num / 1000).toFixed(1)}K`;
  return num.toString();
}

function isAssistantWithOutputTokens(
  message: Message,
): message is AssistantMessage {
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

function sessionTitle(api: TuiApi): string | undefined {
  const sessionID = activeSessionID(api);
  if (!sessionID) return undefined;

  const session = api.state.session.get(sessionID);
  const title = session?.title?.trim();
  if (title) return title;

  const slug = session?.slug?.trim();
  return slug || undefined;
}

function contextUsage(api: TuiApi): string | undefined {
  const sessionID = activeSessionID(api);
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

  const model = api.state.provider.find((item) => item.id === last.providerID)
    ?.models[last.modelID];
  const pct = model?.limit.context
    ? `${Math.round((tokens / model.limit.context) * 100)}%`
    : undefined;
  return pct ? `${formatNumber(tokens)} (${pct})` : formatNumber(tokens);
}

function reportStatus(api: TuiApi, status: KmuxStatus) {
  const sessionID = activeSessionID(api) ?? lastRootSessionID;
  if (!sessionID) return;

  const deleting = status === "clear";
  const title = deleting ? undefined : sessionTitle(api);
  const context = deleting ? undefined : contextUsage(api);
  const paneID = envValue("TMUX_PANE");
  const tmuxInstance = envValue("KMUX_TMUX_SOCKET_NAME");
  const directory = deleting ? undefined : envValue("PWD");
  const instance = deleting
    ? (reportedProducerInstances.get(sessionID) ?? producerInstance())
    : producerInstance();
  const reportKey = JSON.stringify({
    status,
    sessionID,
    title,
    context,
    paneID,
    tmuxInstance,
    directory,
    instance,
  });
  if (lastReportKey === reportKey) return;
  lastReportKey = reportKey;

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
    clearSessionReport(sessionID);
    return;
  } else {
    reportedProducerInstances.set(sessionID, instance);
    currentReportedSessionID = sessionID;
    pushArg(cmd, "--title", title);
    pushArg(cmd, "--context", context);
    pushArg(cmd, "--tmux-instance", tmuxInstance);
    pushArg(cmd, "--pane-id", paneID);
    pushArg(cmd, "--directory", directory);
    pushArg(cmd, "--worktree-path", directory);
  }

  spawnKmux(cmd);
}

function activeRouteSessionID(api: TuiApi) {
  const current = api.route.current;
  const sessionID =
    current.name === "session" ? current.params?.sessionID : undefined;
  return typeof sessionID === "string" ? sessionID : undefined;
}

function activeSessionID(api: TuiApi) {
  const sessionID = activeRouteSessionID(api);
  if (!sessionID) return lastRootSessionID;

  // Subagent routes have their own session IDs, but kmux should track the parent row.
  const session = api.state.session.get(sessionID);
  const rootSessionID = session?.parentID ?? sessionID;
  lastRootSessionID = rootSessionID;
  return rootSessionID;
}

function statusFromCurrentSession(api: TuiApi): KmuxStatus {
  if (!api.state.ready) return "clear";

  const rootSessionID = activeSessionID(api);
  if (!rootSessionID) return "clear";

  const routeSessionID = activeRouteSessionID(api);
  const sessionIDs = new Set([
    ...cachedFamilySessionIDs(api, rootSessionID),
    rootSessionID,
  ]);
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
  const disposers = forwardedEventTypes.map((type) =>
    api.event.on(type, (event) => {
      if (!eventMatchesActiveSession(api, event)) return;
      const status = statusFromEvent(event);
      if (status) {
        recordEventStatus(event, status);
        reportStatus(api, statusFromCurrentSession(api));
      }
    }),
  );
  disposers.push(
    ...metadataEventTypes.map((type) =>
      api.event.on(type, (event) => {
        if (!eventMatchesActiveSession(api, event)) return;
        reportStatus(api, statusFromCurrentSession(api));
      }),
    ),
  );

  reportStatus(api, statusFromCurrentSession(api));

  const timer = setInterval(() => {
    reportStatus(api, statusFromCurrentSession(api));
  }, 300);

  api.lifecycle.onDispose(() => {
    clearInterval(timer);
    for (const dispose of disposers) dispose();
  });
};

const plugin: { id: string; tui: TuiPlugin } = {
  id: "kmux-status",
  tui,
};

export default plugin;
