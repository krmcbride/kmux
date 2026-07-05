/**
 * kmux server-side status tracking for OpenCode.
 *
 * This plugin observes server-side OpenCode events and reports generic agent
 * session state through the kmux CLI. OpenCode-specific session topology stays
 * here; kmux only receives agent/session/status/title/context/target hints.
 */

import type { Plugin } from "@opencode-ai/plugin";
import type {
  GlobalEvent,
  Message,
  Provider,
  Session,
  SessionStatus,
} from "@opencode-ai/sdk";

type PluginInput = Parameters<Plugin>[0];
type OpenCodeClient = PluginInput["client"];
type KmuxStatus = "working" | "waiting" | "done" | "clear";

type SessionInfo = Session & {
  project?: { worktree: string } | null;
  slug?: string;
  workspaceID?: string;
  tokens?: TokenUsage;
};

type TokenUsage = {
  input: number;
  output: number;
  reasoning: number;
  cache: {
    read: number;
    write: number;
  };
};

type AssistantMessage = Extract<Message, { role: "assistant" }>;

declare const Bun: {
  env: Record<string, string | undefined>;
  spawn(input: { cmd: string[]; stdout?: "ignore"; stderr?: "ignore" }): {
    exited: Promise<number>;
  };
};

const AGENT_KIND = "opencode";
const PRODUCER_KIND = "server";
const INITIAL_SESSION_LIMIT = 200;
const REPORT_DEBOUNCE_MS = 150;

function responseData<T>(response: unknown): T | undefined {
  if (response && typeof response === "object" && "data" in response) {
    return (response as { data?: T }).data;
  }
  return response as T | undefined;
}

function clean(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed || undefined;
}

function authHeaders(): Record<string, string> | undefined {
  const password = clean(Bun.env.OPENCODE_SERVER_PASSWORD);
  if (!password) return undefined;

  const username = clean(Bun.env.OPENCODE_SERVER_USERNAME) ?? "opencode";
  return { Authorization: `Basic ${btoa(`${username}:${password}`)}` };
}

function statusFromSessionStatus(
  status: SessionStatus | unknown,
): KmuxStatus | undefined {
  const statusType =
    typeof status === "object" && status !== null && "type" in status
      ? (status as { type?: unknown }).type
      : status;
  if (statusType === "busy" || statusType === "retry") return "working";
  if (statusType === "idle") return "done";
  return undefined;
}

function statusFromEventType(
  type: string,
  properties: unknown,
): KmuxStatus | undefined {
  switch (type) {
    case "session.status":
      return statusFromSessionStatus(
        typeof properties === "object" && properties !== null
          ? (properties as { status?: unknown }).status
          : undefined,
      );
    case "permission.asked":
    case "permission.v2.asked":
    case "permission.updated":
    case "question.asked":
      return "waiting";
    case "permission.replied":
    case "permission.v2.replied":
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

function mostImportantStatus(
  statuses: Iterable<KmuxStatus>,
): KmuxStatus | undefined {
  let best: KmuxStatus = "clear";
  for (const status of statuses) {
    if (statusPriority(status) > statusPriority(best)) best = status;
  }
  return best === "clear" ? undefined : best;
}

function eventSessionID(properties: unknown): string | undefined {
  if (typeof properties !== "object" || properties === null) return undefined;
  const direct = clean((properties as { sessionID?: unknown }).sessionID);
  if (direct) return direct;

  const info = (properties as { info?: unknown }).info;
  if (typeof info !== "object" || info === null) return undefined;
  return (
    clean((info as { sessionID?: unknown }).sessionID) ??
    clean((info as { id?: unknown }).id)
  );
}

function eventSessionInfo(properties: unknown): SessionInfo | undefined {
  if (typeof properties !== "object" || properties === null) return undefined;
  const info = (properties as { info?: unknown }).info;
  if (typeof info !== "object" || info === null) return undefined;
  if (typeof (info as { id?: unknown }).id !== "string") return undefined;
  return info as SessionInfo;
}

function formatNumber(num: number): string {
  if (num >= 1000000) return `${(num / 1000000).toFixed(1)}M`;
  if (num >= 1000) return `${(num / 1000).toFixed(1)}K`;
  return num.toString();
}

function messageTokens(message: AssistantMessage): number | undefined {
  if (message.tokens.output <= 0) return undefined;
  return (
    message.tokens.input +
    message.tokens.output +
    message.tokens.reasoning +
    message.tokens.cache.read +
    message.tokens.cache.write
  );
}

function isAssistantWithOutputTokens(
  message: Message,
): message is AssistantMessage {
  return message.role === "assistant" && message.tokens.output > 0;
}

function lastAssistantMessageWithTokens(
  messages: ReadonlyArray<Message>,
): AssistantMessage | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message && isAssistantWithOutputTokens(message)) return message;
  }
}

function sessionTokens(session: SessionInfo): number | undefined {
  const tokens = session.tokens;
  if (!tokens) return undefined;
  const total =
    tokens.input +
    tokens.output +
    tokens.reasoning +
    tokens.cache.read +
    tokens.cache.write;
  return total > 0 ? total : undefined;
}

class KmuxServerReporter {
  private readonly client: OpenCodeClient;
  private readonly serverUrl: URL;
  private readonly instance: string;
  private readonly sessions = new Map<string, SessionInfo>();
  private readonly statuses = new Map<string, KmuxStatus>();
  private readonly lastReport = new Map<string, string>();
  private readonly reportedRoots = new Set<string>();
  private readonly modelLimits = new Map<
    string,
    Promise<Map<string, number>>
  >();
  private readonly pendingReports = new Map<
    string,
    ReturnType<typeof setTimeout>
  >();
  private readonly abort = new AbortController();
  private disposed = false;

  constructor(input: PluginInput) {
    this.client = input.client;
    this.serverUrl = input.serverUrl;
    this.instance = input.serverUrl.toString().replace(/\/$/, "") || "default";
  }

  async start() {
    await this.bootstrap();
    void this.streamEvents();
  }

  async dispose() {
    this.disposed = true;
    this.abort.abort();
    for (const timer of this.pendingReports.values()) clearTimeout(timer);
    this.pendingReports.clear();
    for (const rootID of this.reportedRoots) this.clearReport(rootID);
  }

  private async bootstrap() {
    const sessions = await this.bootstrapSessions();
    for (const session of sessions) this.recordSession(session);

    const statusResponse = await this.client.session
      .status()
      .catch(() => undefined);
    const statuses =
      responseData<Record<string, SessionStatus>>(statusResponse) ?? {};
    const rootIDs = new Set<string>();
    for (const [sessionID, status] of Object.entries(statuses)) {
      const kmuxStatus = statusFromSessionStatus(status);
      if (!kmuxStatus) continue;
      this.recordStatus(sessionID, kmuxStatus);
      rootIDs.add(this.rootSessionID(sessionID));
    }
    for (const rootID of rootIDs) {
      if (this.rootStatus(rootID) === "done") this.clearReport(rootID);
      else this.scheduleReport(rootID);
    }
  }

  private async bootstrapSessions(): Promise<SessionInfo[]> {
    const url = new URL("/experimental/session", this.serverUrl);
    url.searchParams.set("limit", String(INITIAL_SESSION_LIMIT));
    const headers = authHeaders();
    const response = await fetch(url, {
      signal: this.abort.signal,
      ...(headers ? { headers } : {}),
    }).catch(() => undefined);
    if (!response?.ok) return [];
    const data = await response.json().catch(() => undefined);
    return Array.isArray(data) ? (data as SessionInfo[]) : [];
  }

  private async streamEvents() {
    while (!this.disposed) {
      const events = await this.client.global
        .event({ signal: this.abort.signal })
        .catch(() => undefined);
      if (!events) {
        if (this.disposed) return;
        await new Promise((resolve) => setTimeout(resolve, 1000));
        continue;
      }

      try {
        for await (const event of events.stream as AsyncGenerator<GlobalEvent>) {
          if (this.disposed) return;
          await this.handleGlobalEvent(event);
        }
      } catch {
        if (this.disposed) return;
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
  }

  private async handleGlobalEvent(event: GlobalEvent) {
    const payload = event.payload as
      | { type?: string; properties?: unknown }
      | undefined;
    if (!payload || payload.type === "sync" || !payload.type) return;

    const properties = "properties" in payload ? payload.properties : undefined;
    const info = eventSessionInfo(properties);
    if (info) this.recordSession(info, event.directory);

    if (payload.type === "session.deleted") {
      const sessionID = eventSessionID(properties);
      if (!sessionID) return;
      const rootID = this.rootSessionID(sessionID);
      this.deleteSession(sessionID);
      if (sessionID === rootID) this.clearReport(rootID, true);
      else this.scheduleReport(rootID);
      return;
    }

    const sessionID = eventSessionID(properties);
    const status = statusFromEventType(payload.type, properties);
    if (sessionID && status) {
      this.recordStatus(sessionID, status);
      this.scheduleReport(this.rootSessionID(sessionID));
      return;
    }

    if (
      sessionID &&
      (payload.type === "session.created" ||
        payload.type === "session.updated" ||
        payload.type === "message.updated" ||
        payload.type === "message.removed")
    ) {
      this.scheduleReport(this.rootSessionID(sessionID));
    }
  }

  private recordSession(session: SessionInfo, fallbackDirectory?: string) {
    const oldRoots = this.statusRoots();
    const previous = this.sessions.get(session.id);
    this.sessions.set(session.id, {
      ...previous,
      ...session,
      directory:
        clean(session.directory) ??
        clean(fallbackDirectory) ??
        previous?.directory ??
        "",
      workspaceID: clean(session.workspaceID),
      project: "project" in session ? session.project : previous?.project,
    });
    this.reconcileChangedRoots(oldRoots);
  }

  private deleteSession(sessionID: string) {
    const deletedSessionIDs = new Set(
      [...this.sessions.keys()].filter(
        (storedSessionID) =>
          storedSessionID === sessionID ||
          this.hasAncestor(storedSessionID, sessionID),
      ),
    );
    deletedSessionIDs.add(sessionID);
    const deletedStatusIDs = [...this.statuses.keys()].filter(
      (statusSessionID) =>
        deletedSessionIDs.has(statusSessionID) ||
        this.hasAncestor(statusSessionID, sessionID),
    );
    for (const deletedSessionID of deletedSessionIDs)
      this.sessions.delete(deletedSessionID);
    for (const statusSessionID of deletedStatusIDs)
      this.statuses.delete(statusSessionID);
  }

  private hasAncestor(sessionID: string, ancestorID: string): boolean {
    let current = sessionID;
    const seen = new Set<string>();
    while (!seen.has(current)) {
      seen.add(current);
      const parentID = clean(this.sessions.get(current)?.parentID);
      if (!parentID) return false;
      if (parentID === ancestorID) return true;
      current = parentID;
    }
    return false;
  }

  private rootSessionID(sessionID: string): string {
    let current = sessionID;
    const seen = new Set<string>();
    while (!seen.has(current)) {
      seen.add(current);
      const parentID = clean(this.sessions.get(current)?.parentID);
      if (!parentID) return current;
      current = parentID;
    }
    return sessionID;
  }

  private recordStatus(sessionID: string, status: KmuxStatus) {
    this.statuses.set(sessionID, status);
  }

  private statusRoots(): Map<string, string> {
    const roots = new Map<string, string>();
    for (const sessionID of this.statuses.keys())
      roots.set(sessionID, this.rootSessionID(sessionID));
    return roots;
  }

  private reconcileChangedRoots(oldRoots: Map<string, string>) {
    for (const sessionID of this.statuses.keys()) {
      const oldRootID = oldRoots.get(sessionID);
      const rootID = this.rootSessionID(sessionID);
      if (oldRootID && oldRootID !== rootID) {
        this.clearReport(oldRootID);
        this.scheduleReport(rootID);
      }
    }
  }

  private scheduleReport(rootID: string) {
    if (this.disposed) return;
    const existing = this.pendingReports.get(rootID);
    if (existing) clearTimeout(existing);
    this.pendingReports.set(
      rootID,
      setTimeout(() => {
        this.pendingReports.delete(rootID);
        void this.reportRoot(rootID);
      }, REPORT_DEBOUNCE_MS),
    );
  }

  private async reportRoot(rootID: string) {
    const root = this.sessions.get(rootID);
    if (!root) return;
    const status = this.rootStatus(rootID);
    if (!status) {
      this.clearReport(rootID);
      return;
    }

    const title = clean(root.title) ?? clean(root.slug);
    const context = await this.contextUsage(root);
    const directory = clean(root.directory) ?? clean(root.project?.worktree);
    const workspaceID = clean(root.workspaceID);
    const reportKey = JSON.stringify({
      status,
      title,
      context,
      directory,
      workspaceID,
    });
    if (this.lastReport.get(rootID) === reportKey) return;
    this.lastReport.set(rootID, reportKey);
    this.reportedRoots.add(rootID);

    const cmd = [
      "kmux",
      "set-agent-status",
      status,
      "--agent-kind",
      AGENT_KIND,
      "--session-id",
      rootID,
      "--producer-kind",
      PRODUCER_KIND,
      "--producer-instance",
      this.instance,
    ];
    if (title) cmd.push("--title", title);
    if (context) cmd.push("--context", context);
    if (workspaceID) cmd.push("--agent-workspace-id", workspaceID);
    else cmd.push("--clear-agent-workspace-id");
    if (directory) cmd.push("--directory", directory);

    this.spawnKmux(cmd);
  }

  private rootStatus(rootID: string): KmuxStatus | undefined {
    return mostImportantStatus(
      [...this.statuses]
        .filter(([sessionID]) => this.rootSessionID(sessionID) === rootID)
        .map(([, status]) => status),
    );
  }

  private clearReport(rootID: string, deleteSession = false) {
    this.lastReport.delete(rootID);
    this.reportedRoots.delete(rootID);
    this.spawnKmux([
      "kmux",
      "set-agent-status",
      "--agent-kind",
      AGENT_KIND,
      "--session-id",
      rootID,
      "--producer-kind",
      PRODUCER_KIND,
      "--producer-instance",
      this.instance,
      deleteSession ? "--delete-session" : "--delete",
    ]);
  }

  private async contextUsage(root: SessionInfo): Promise<string | undefined> {
    const messagesResponse = await this.client.session
      .messages({
        path: { id: root.id },
        query: {
          directory: root.directory,
          limit: 50,
        },
      })
      .catch(() => undefined);
    const messages = (
      responseData<{ info: Message }[]>(messagesResponse) ?? []
    ).map((message) => message.info);
    const lastAssistant = lastAssistantMessageWithTokens(messages);
    const tokens = lastAssistant
      ? messageTokens(lastAssistant)
      : sessionTokens(root);
    if (!tokens) return undefined;

    const limit = lastAssistant
      ? await this.contextLimit(
          root.directory,
          lastAssistant.providerID,
          lastAssistant.modelID,
        )
      : undefined;
    const pct = limit ? `${Math.round((tokens / limit) * 100)}%` : undefined;
    return pct ? `${formatNumber(tokens)} (${pct})` : formatNumber(tokens);
  }

  private async contextLimit(
    directory: string | undefined,
    providerID: string,
    modelID: string,
  ): Promise<number | undefined> {
    const limits = await this.contextLimits(directory).catch(() => undefined);
    return limits?.get(`${providerID}/${modelID}`);
  }

  private contextLimits(directory: string | undefined) {
    const key = clean(directory) ?? "";
    let limits = this.modelLimits.get(key);
    if (!limits) {
      limits = this.loadContextLimits(key || undefined);
      this.modelLimits.set(key, limits);
    }
    return limits;
  }

  private async loadContextLimits(directory: string | undefined) {
    const response = await this.client.config.providers(
      directory ? { query: { directory } } : undefined,
    );
    const providers =
      responseData<{ providers: Provider[] }>(response)?.providers ?? [];
    const limits = new Map<string, number>();
    for (const provider of providers) {
      for (const [modelID, model] of Object.entries(provider.models)) {
        const limit = model.limit.context;
        if (limit > 0) limits.set(`${provider.id}/${modelID}`, limit);
      }
    }
    return limits;
  }

  private spawnKmux(cmd: string[]) {
    try {
      void Bun.spawn({ cmd, stdout: "ignore", stderr: "ignore" }).exited;
    } catch {
      // Keep OpenCode usable if kmux is unavailable in this server environment.
    }
  }
}

const server: Plugin = async (input) => {
  const reporter = new KmuxServerReporter(input);
  void reporter.start().catch(() => {});
  return {
    dispose: () => reporter.dispose(),
  };
};

export default {
  id: "kmux-status-server",
  server,
};
