/** Directory-scoped OpenCode session policy and kmux report delivery. */

import {
  KmuxCommandQueue,
  type KmuxCommandResult,
  type KmuxCommandRunner,
} from "./kmux-command-queue";

export type ReporterEvent = {
  type: string;
  properties?: unknown;
};

export type TokenUsage = {
  input: number;
  output: number;
  reasoning: number;
  cache: {
    read: number;
    write: number;
  };
};

export type ReporterSession = {
  id: string;
  parentID?: string;
  directory?: string;
  title?: string;
  slug?: string;
  tokens?: TokenUsage;
};

export type ReporterMessage = {
  role: string;
  providerID?: string;
  modelID?: string;
  tokens?: TokenUsage;
};

export type ReporterSource = {
  sessions(signal: AbortSignal): Promise<ReadonlyArray<ReporterSession>>;
  statuses(signal: AbortSignal): Promise<Readonly<Record<string, unknown>>>;
  messages(session: ReporterSession, signal: AbortSignal): Promise<ReadonlyArray<ReporterMessage>>;
  modelLimits(
    directory: string | undefined,
    signal: AbortSignal,
  ): Promise<ReadonlyMap<string, number>>;
};

export type ReporterLogEntry = {
  level: "debug" | "info" | "warn" | "error";
  message: string;
  extra?: Record<string, unknown>;
};

export type ReporterLogger = (entry: ReporterLogEntry) => void;

export type ReporterScheduler = {
  schedule(delayMs: number, task: () => void): () => void;
};

export type KmuxServerReporterDependencies = {
  producerInstance: string;
  fallbackDirectory: string;
  source: ReporterSource;
  runCommand: KmuxCommandRunner;
  log: ReporterLogger;
  scheduler?: ReporterScheduler;
  reportDebounceMs?: number;
  bootstrapTimeoutMs?: number;
  contextTimeoutMs?: number;
  disposeTimeoutMs?: number;
};

type KmuxStatus = "working" | "waiting" | "done" | "clear";

type ScheduledReport = {
  revision: number;
  cancel: () => void;
};

type DeliveryIntent = {
  id: number;
  key: string;
  operation: "set" | "clear" | "delete-session";
  rootID: string;
};

type DeliveryState = {
  pending?: DeliveryIntent;
  successfulKey?: string;
};

const AGENT_KIND = "opencode";
const PRODUCER_KIND = "server";
const DEFAULT_REPORT_DEBOUNCE_MS = 150;
const DEFAULT_BOOTSTRAP_TIMEOUT_MS = 2000;
const DEFAULT_CONTEXT_TIMEOUT_MS = 500;
const DEFAULT_DISPOSE_TIMEOUT_MS = 3000;

const nativeScheduler: ReporterScheduler = {
  schedule(delayMs, task) {
    const timer = setTimeout(task, Math.max(0, delayMs));
    return () => clearTimeout(timer);
  },
};

/**
 * Own one OpenCode directory's session family state and ordered kmux reports.
 *
 * Bootstrap events are buffered and replayed after the snapshot so newer hook
 * events always win. All public lifecycle methods contain their own error
 * boundaries because OpenCode does not supervise event-hook promises.
 */
export class KmuxServerReporter {
  private readonly producerInstance: string;
  private readonly fallbackDirectory: string;
  private readonly source: ReporterSource;
  private readonly logTransport: ReporterLogger;
  private readonly scheduler: ReporterScheduler;
  private readonly reportDebounceMs: number;
  private readonly bootstrapTimeoutMs: number;
  private readonly contextTimeoutMs: number;
  private readonly disposeTimeoutMs: number;
  private readonly commandQueue: KmuxCommandQueue;
  private readonly sessions = new Map<string, ReporterSession>();
  private readonly statuses = new Map<string, KmuxStatus>();
  private readonly pendingReports = new Map<string, ScheduledReport>();
  private readonly reportRevisions = new Map<string, number>();
  private readonly deliveryStates = new Map<string, DeliveryState>();
  private readonly managedRoots = new Set<string>();
  private readonly modelLimits = new Map<string, ReadonlyMap<string, number>>();
  private readonly modelLimitRequests = new Map<string, number>();
  private readonly commandFailureSignatures = new Map<string, string>();
  private readonly contextTimeoutRoots = new Set<string>();
  private readonly operationAbort = new AbortController();
  private readonly bootstrapAbort = new AbortController();
  private bufferedEvents: ReporterEvent[] = [];
  private bootstrapState: "idle" | "running" | "ready" = "idle";
  private startupPromise?: Promise<void>;
  private disposePromise?: Promise<void>;
  private nextReportRevision = 0;
  private nextCommandID = 0;
  private nextModelLimitRequest = 0;
  private bootstrapTimedOut = false;
  private disposed = false;

  constructor(dependencies: KmuxServerReporterDependencies) {
    this.producerInstance = dependencies.producerInstance;
    this.fallbackDirectory = dependencies.fallbackDirectory;
    this.source = dependencies.source;
    this.logTransport = dependencies.log;
    this.scheduler = dependencies.scheduler ?? nativeScheduler;
    this.reportDebounceMs = dependencies.reportDebounceMs ?? DEFAULT_REPORT_DEBOUNCE_MS;
    this.bootstrapTimeoutMs = dependencies.bootstrapTimeoutMs ?? DEFAULT_BOOTSTRAP_TIMEOUT_MS;
    this.contextTimeoutMs = dependencies.contextTimeoutMs ?? DEFAULT_CONTEXT_TIMEOUT_MS;
    this.disposeTimeoutMs = dependencies.disposeTimeoutMs ?? DEFAULT_DISPOSE_TIMEOUT_MS;
    this.commandQueue = new KmuxCommandQueue(dependencies.runCommand);
  }

  /** Begin bounded snapshot loading while hook events are buffered. */
  start(): Promise<void> {
    if (this.startupPromise) {
      return this.startupPromise;
    }
    if (this.disposed) {
      return Promise.resolve();
    }

    this.bootstrapState = "running";
    this.startupPromise = this.bootstrap().catch((error: unknown) => {
      this.log({
        level: "error",
        message: "Unexpected reporter bootstrap failure",
        extra: { operation: "bootstrap", error: errorMetadata(error) },
      });
      this.finishBootstrap();
    });
    return this.startupPromise;
  }

  /** Accept one directory-scoped OpenCode hook event without rejecting the host. */
  async event(event: ReporterEvent): Promise<void> {
    try {
      if (this.disposed) {
        return;
      }
      if (this.bootstrapState !== "ready") {
        this.bufferedEvents.push(event);
        return;
      }
      this.applyEvent(event);
    } catch (error) {
      this.log({
        level: "error",
        message: "OpenCode event handling failed",
        extra: { operation: "event", eventType: event.type, error: errorMetadata(error) },
      });
    }
  }

  /** Stop event intake, enqueue owned-observation cleanup, and drain for a bounded time. */
  dispose(): Promise<void> {
    if (!this.disposePromise) {
      this.disposePromise = this.disposeInternal();
    }
    return this.disposePromise;
  }

  private async bootstrap(): Promise<void> {
    let sessions: ReadonlyArray<ReporterSession> = [];
    let statuses: Readonly<Record<string, unknown>> = {};
    const signal = this.bootstrapAbort.signal;
    const loading = Promise.all([
      this.source.sessions(signal).then(
        (value) => {
          sessions = value;
        },
        (error: unknown) => {
          if (!this.disposed && !this.bootstrapTimedOut) {
            this.log({
              level: "warn",
              message: "OpenCode session bootstrap failed",
              extra: { operation: "bootstrap-sessions", error: errorMetadata(error) },
            });
          }
        },
      ),
      this.source.statuses(signal).then(
        (value) => {
          statuses = value;
        },
        (error: unknown) => {
          if (!this.disposed && !this.bootstrapTimedOut) {
            this.log({
              level: "warn",
              message: "OpenCode status bootstrap failed",
              extra: { operation: "bootstrap-statuses", error: errorMetadata(error) },
            });
          }
        },
      ),
    ]);
    const completed = await settleWithin(loading, this.bootstrapTimeoutMs);
    if (!completed) {
      this.bootstrapTimedOut = true;
      this.bootstrapAbort.abort();
      this.log({
        level: "warn",
        message: "OpenCode reporter bootstrap timed out",
        extra: { operation: "bootstrap", timeoutMs: this.bootstrapTimeoutMs },
      });
    }
    if (this.disposed) {
      return;
    }

    for (const session of sessions) {
      this.recordSession(session);
    }
    const rootIDs = new Set<string>();
    for (const [sessionID, statusValue] of Object.entries(statuses)) {
      const status = statusFromSessionStatus(statusValue);
      if (!status) {
        continue;
      }
      this.recordStatus(sessionID, status);
      rootIDs.add(this.rootSessionID(sessionID));
    }
    for (const rootID of rootIDs) {
      if (this.rootStatus(rootID) === "done") {
        this.clearReport(rootID);
      } else {
        this.scheduleReport(rootID);
      }
    }
    this.finishBootstrap();
  }

  private finishBootstrap() {
    if (this.disposed || this.bootstrapState === "ready") {
      this.bufferedEvents = [];
      return;
    }
    this.bootstrapState = "ready";
    const events = this.bufferedEvents;
    this.bufferedEvents = [];
    for (const event of events) {
      try {
        this.applyEvent(event);
      } catch (error) {
        this.log({
          level: "error",
          message: "Buffered OpenCode event handling failed",
          extra: { operation: "event", eventType: event.type, error: errorMetadata(error) },
        });
      }
    }
  }

  private applyEvent(event: ReporterEvent) {
    if (
      event.type === "session.created" ||
      event.type === "session.updated" ||
      event.type === "session.deleted"
    ) {
      const info = eventSessionInfo(event.properties);
      if (info) {
        this.recordSession(info);
      }
    }

    if (event.type === "session.deleted") {
      const sessionID = eventSessionID(event.properties);
      if (!sessionID) {
        return;
      }
      const rootID = this.rootSessionID(sessionID);
      this.deleteSession(sessionID);
      if (sessionID === rootID) {
        this.clearReport(rootID, true);
      } else {
        this.scheduleReport(rootID);
      }
      return;
    }

    const sessionID = eventSessionID(event.properties);
    const status = statusFromEventType(event.type, event.properties);
    if (sessionID && status) {
      this.recordStatus(sessionID, status);
      this.scheduleReport(this.rootSessionID(sessionID));
      return;
    }

    if (
      sessionID &&
      (event.type === "session.created" ||
        event.type === "session.updated" ||
        event.type === "message.updated" ||
        event.type === "message.removed")
    ) {
      this.scheduleReport(this.rootSessionID(sessionID));
    }
  }

  private recordSession(session: ReporterSession) {
    const oldRoots = this.statusRoots();
    const previous = this.sessions.get(session.id);
    this.sessions.set(session.id, {
      ...previous,
      ...session,
      directory:
        clean(session.directory) ?? previous?.directory ?? clean(this.fallbackDirectory) ?? "",
    });
    this.reconcileChangedRoots(oldRoots);
  }

  private deleteSession(sessionID: string) {
    const deletedSessionIDs = new Set(
      [...this.sessions.keys()].filter(
        (storedSessionID) =>
          storedSessionID === sessionID || this.hasAncestor(storedSessionID, sessionID),
      ),
    );
    deletedSessionIDs.add(sessionID);
    const deletedStatusIDs = [...this.statuses.keys()].filter(
      (statusSessionID) =>
        deletedSessionIDs.has(statusSessionID) || this.hasAncestor(statusSessionID, sessionID),
    );
    for (const deletedSessionID of deletedSessionIDs) {
      this.sessions.delete(deletedSessionID);
    }
    for (const statusSessionID of deletedStatusIDs) {
      this.statuses.delete(statusSessionID);
    }
  }

  private hasAncestor(sessionID: string, ancestorID: string): boolean {
    let current = sessionID;
    const seen = new Set<string>();
    while (!seen.has(current)) {
      seen.add(current);
      const parentID = clean(this.sessions.get(current)?.parentID);
      if (!parentID) {
        return false;
      }
      if (parentID === ancestorID) {
        return true;
      }
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
      if (!parentID) {
        return current;
      }
      current = parentID;
    }
    return sessionID;
  }

  private recordStatus(sessionID: string, status: KmuxStatus) {
    this.statuses.set(sessionID, status);
  }

  private statusRoots(): Map<string, string> {
    const roots = new Map<string, string>();
    for (const sessionID of this.statuses.keys()) {
      roots.set(sessionID, this.rootSessionID(sessionID));
    }
    return roots;
  }

  private reconcileChangedRoots(oldRoots: Map<string, string>) {
    const changedOldRoots = new Set<string>();
    const changedNewRoots = new Set<string>();
    for (const sessionID of this.statuses.keys()) {
      const oldRootID = oldRoots.get(sessionID);
      const rootID = this.rootSessionID(sessionID);
      if (oldRootID && oldRootID !== rootID) {
        changedOldRoots.add(oldRootID);
        changedNewRoots.add(rootID);
      }
    }
    for (const oldRootID of changedOldRoots) {
      if (this.rootStatus(oldRootID)) {
        this.scheduleReport(oldRootID);
      } else {
        this.clearReport(oldRootID);
      }
    }
    for (const rootID of changedNewRoots) {
      this.scheduleReport(rootID);
    }
  }

  private scheduleReport(rootID: string) {
    if (this.disposed) {
      return;
    }
    const revision = ++this.nextReportRevision;
    this.reportRevisions.set(rootID, revision);
    this.pendingReports.get(rootID)?.cancel();
    const cancel = this.scheduler.schedule(this.reportDebounceMs, () => {
      const pending = this.pendingReports.get(rootID);
      if (pending?.revision !== revision) {
        return;
      }
      this.pendingReports.delete(rootID);
      void this.reportRoot(rootID, revision).catch((error: unknown) => {
        this.log({
          level: "error",
          message: "Session report calculation failed",
          extra: { operation: "report", sessionID: rootID, error: errorMetadata(error) },
        });
      });
    });
    this.pendingReports.set(rootID, { revision, cancel });
  }

  private async reportRoot(rootID: string, revision: number) {
    if (this.disposed) {
      return;
    }
    const root = this.sessions.get(rootID);
    if (!root) {
      return;
    }
    const status = this.rootStatus(rootID);
    if (!status) {
      this.clearReport(rootID);
      return;
    }

    const title = clean(root.title) ?? clean(root.slug);
    const context = await this.boundedContextUsage(root);
    if (this.disposed || this.reportRevisions.get(rootID) !== revision) {
      return;
    }
    const directory = clean(root.directory) ?? clean(this.fallbackDirectory);
    const reportKey = JSON.stringify({ operation: "set", status, title, context, directory });
    const command = [
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
      this.producerInstance,
    ];
    if (title) {
      command.push("--title", title);
    }
    if (context) {
      command.push("--context", context);
    }
    if (directory) {
      command.push("--directory", directory);
    }
    this.enqueueDelivery(rootID, "set", reportKey, command);
  }

  private rootStatus(rootID: string): KmuxStatus | undefined {
    return mostImportantStatus(
      [...this.statuses]
        .filter(([sessionID]) => this.rootSessionID(sessionID) === rootID)
        .map(([, status]) => status),
    );
  }

  private clearReport(rootID: string, deleteSession = false) {
    this.pendingReports.get(rootID)?.cancel();
    this.pendingReports.delete(rootID);
    this.reportRevisions.delete(rootID);
    const operation = deleteSession ? "delete-session" : "clear";
    const key = JSON.stringify({ operation });
    this.enqueueDelivery(
      rootID,
      operation,
      key,
      [
        "kmux",
        "set-agent-status",
        "--agent-kind",
        AGENT_KIND,
        "--session-id",
        rootID,
        "--producer-kind",
        PRODUCER_KIND,
        "--producer-instance",
        this.producerInstance,
        deleteSession ? "--delete-session" : "--delete",
      ],
      this.disposed,
    );
  }

  private enqueueDelivery(
    rootID: string,
    operation: DeliveryIntent["operation"],
    key: string,
    command: ReadonlyArray<string>,
    allowDisposed = false,
  ) {
    if (this.disposed && !allowDisposed) {
      return;
    }
    const state = this.deliveryStates.get(rootID) ?? {};
    if (state.pending?.key === key || (!state.pending && state.successfulKey === key)) {
      return;
    }

    const intent: DeliveryIntent = {
      id: ++this.nextCommandID,
      key,
      operation,
      rootID,
    };
    state.pending = intent;
    this.deliveryStates.set(rootID, state);
    if (operation === "set") {
      this.managedRoots.add(rootID);
    }
    void this.commandQueue.enqueue(command).then((result) => this.completeDelivery(intent, result));
  }

  private completeDelivery(intent: DeliveryIntent, result: KmuxCommandResult) {
    if (!result.ok) {
      this.logCommandFailure(intent, result);
      if (this.disposed) {
        return;
      }
      const state = this.deliveryStates.get(intent.rootID);
      if (state?.pending?.id === intent.id) {
        state.pending = undefined;
      }
      return;
    }

    const failureKey = `${intent.rootID}:${intent.operation}`;
    if (this.commandFailureSignatures.delete(failureKey)) {
      this.log({
        level: "info",
        message: "kmux command delivery recovered",
        extra: {
          operation: intent.operation,
          sessionID: intent.rootID,
          commandID: intent.id,
        },
      });
    }
    if (this.disposed) {
      return;
    }
    const state = this.deliveryStates.get(intent.rootID) ?? {};
    state.successfulKey = intent.key;
    if (state.pending?.id === intent.id) {
      state.pending = undefined;
    }
    this.deliveryStates.set(intent.rootID, state);
    if (intent.operation === "set") {
      this.managedRoots.add(intent.rootID);
    } else if (state.pending?.operation !== "set") {
      // A newer set can already be queued behind this successful clear. Keep
      // ownership until that set and its eventual cleanup have settled.
      this.managedRoots.delete(intent.rootID);
      this.contextTimeoutRoots.delete(intent.rootID);
      if (!state.pending) {
        this.deliveryStates.delete(intent.rootID);
        this.commandFailureSignatures.delete(`${intent.rootID}:set`);
        this.commandFailureSignatures.delete(`${intent.rootID}:clear`);
        this.commandFailureSignatures.delete(`${intent.rootID}:delete-session`);
      }
    }
  }

  private logCommandFailure(
    intent: DeliveryIntent,
    result: Exclude<KmuxCommandResult, { ok: true }>,
  ) {
    const signature =
      "exitCode" in result ? `exit:${result.exitCode}` : errorSignature(result.error);
    const failureKey = `${intent.rootID}:${intent.operation}`;
    if (this.commandFailureSignatures.get(failureKey) === signature) {
      return;
    }
    this.commandFailureSignatures.set(failureKey, signature);
    this.log({
      level: "warn",
      message: "kmux command delivery failed",
      extra: {
        operation: intent.operation,
        sessionID: intent.rootID,
        commandID: intent.id,
        ...("exitCode" in result
          ? { exitCode: result.exitCode }
          : { error: errorMetadata(result.error) }),
      },
    });
  }

  private async boundedContextUsage(root: ReporterSession): Promise<string | undefined> {
    const contextAbort = new AbortController();
    const abortContext = () => contextAbort.abort();
    if (this.operationAbort.signal.aborted) {
      contextAbort.abort();
    } else {
      this.operationAbort.signal.addEventListener("abort", abortContext, { once: true });
    }
    let result: Awaited<ReturnType<typeof settleValueWithin<string | undefined>>>;
    try {
      result = await settleValueWithin(
        this.contextUsage(root, contextAbort.signal),
        this.contextTimeoutMs,
      );
    } finally {
      this.operationAbort.signal.removeEventListener("abort", abortContext);
    }
    if (!result.completed) {
      contextAbort.abort();
      if (!this.contextTimeoutRoots.has(root.id)) {
        this.contextTimeoutRoots.add(root.id);
        this.log({
          level: "debug",
          message: "OpenCode context enrichment timed out",
          extra: {
            operation: "context",
            sessionID: root.id,
            timeoutMs: this.contextTimeoutMs,
          },
        });
      }
      return undefined;
    }
    this.contextTimeoutRoots.delete(root.id);
    return result.value;
  }

  private async contextUsage(
    root: ReporterSession,
    signal: AbortSignal,
  ): Promise<string | undefined> {
    let messages: ReadonlyArray<ReporterMessage> = [];
    try {
      messages = await this.source.messages(root, signal);
    } catch (error) {
      if (!this.disposed && !signal.aborted) {
        this.log({
          level: "debug",
          message: "OpenCode context messages unavailable",
          extra: { operation: "messages", sessionID: root.id, error: errorMetadata(error) },
        });
      }
    }
    const lastAssistant = lastAssistantMessageWithTokens(messages);
    const tokens = lastAssistant ? messageTokens(lastAssistant) : sessionTokens(root);
    if (!tokens) {
      return undefined;
    }

    const limit = lastAssistant
      ? await this.contextLimit(
          root.directory,
          lastAssistant.providerID,
          lastAssistant.modelID,
          signal,
        )
      : undefined;
    const percentage = limit ? `${Math.round((tokens / limit) * 100)}%` : undefined;
    return percentage ? `${formatNumber(tokens)} (${percentage})` : formatNumber(tokens);
  }

  private async contextLimit(
    directory: string | undefined,
    providerID: string | undefined,
    modelID: string | undefined,
    signal: AbortSignal,
  ): Promise<number | undefined> {
    if (!providerID || !modelID) {
      return undefined;
    }
    try {
      const limits = await this.contextLimits(directory, signal);
      return limits.get(`${providerID}/${modelID}`);
    } catch (error) {
      if (!this.disposed && !signal.aborted) {
        this.log({
          level: "debug",
          message: "OpenCode model limits unavailable",
          extra: { operation: "providers", error: errorMetadata(error) },
        });
      }
      return undefined;
    }
  }

  private async contextLimits(directory: string | undefined, signal: AbortSignal) {
    const key = clean(directory) ?? "";
    let limits = this.modelLimits.get(key);
    if (!limits) {
      const requestID = ++this.nextModelLimitRequest;
      this.modelLimitRequests.set(key, requestID);
      limits = await this.source.modelLimits(directory, signal);
      if (this.modelLimitRequests.get(key) === requestID) {
        this.modelLimits.set(key, limits);
      }
    }
    return limits;
  }

  private async disposeInternal() {
    this.disposed = true;
    this.bootstrapAbort.abort();
    this.operationAbort.abort();
    this.bufferedEvents = [];
    for (const report of this.pendingReports.values()) {
      report.cancel();
    }
    this.pendingReports.clear();
    this.reportRevisions.clear();

    for (const rootID of [...this.managedRoots]) {
      this.clearReport(rootID);
    }
    const drained = await this.commandQueue.drain(this.disposeTimeoutMs);
    if (!drained) {
      this.log({
        level: "warn",
        message: "Timed out draining kmux commands during disposal",
        extra: { operation: "dispose", timeoutMs: this.disposeTimeoutMs },
      });
    }
  }

  private log(entry: ReporterLogEntry) {
    try {
      this.logTransport(entry);
    } catch {
      // Logging is the final diagnostic boundary and must never break OpenCode.
    }
  }
}

function clean(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function statusFromSessionStatus(status: unknown): KmuxStatus | undefined {
  const statusType =
    typeof status === "object" && status !== null && "type" in status
      ? (status as { type?: unknown }).type
      : status;
  if (statusType === "busy" || statusType === "retry") {
    return "working";
  }
  if (statusType === "idle") {
    return "done";
  }
  return undefined;
}

function statusFromEventType(type: string, properties: unknown): KmuxStatus | undefined {
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
    case "question.v2.asked":
      return "waiting";
    case "permission.replied":
    case "permission.v2.replied":
    case "question.replied":
    case "question.rejected":
    case "question.v2.replied":
    case "question.v2.rejected":
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

function mostImportantStatus(statuses: Iterable<KmuxStatus>): KmuxStatus | undefined {
  let best: KmuxStatus = "clear";
  for (const status of statuses) {
    if (statusPriority(status) > statusPriority(best)) {
      best = status;
    }
  }
  return best === "clear" ? undefined : best;
}

function eventSessionID(properties: unknown): string | undefined {
  if (typeof properties !== "object" || properties === null) {
    return undefined;
  }
  const direct = clean((properties as { sessionID?: unknown }).sessionID);
  if (direct) {
    return direct;
  }
  const info = (properties as { info?: unknown }).info;
  if (typeof info !== "object" || info === null) {
    return undefined;
  }
  return clean((info as { sessionID?: unknown }).sessionID) ?? clean((info as { id?: unknown }).id);
}

function eventSessionInfo(properties: unknown): ReporterSession | undefined {
  if (typeof properties !== "object" || properties === null) {
    return undefined;
  }
  const info = (properties as { info?: unknown }).info;
  if (typeof info !== "object" || info === null || !clean((info as { id?: unknown }).id)) {
    return undefined;
  }
  return info as ReporterSession;
}

function formatNumber(num: number): string {
  if (num >= 1000000) {
    return `${(num / 1000000).toFixed(1)}M`;
  }
  if (num >= 1000) {
    return `${(num / 1000).toFixed(1)}K`;
  }
  return num.toString();
}

function messageTokens(message: ReporterMessage): number | undefined {
  const tokens = message.tokens;
  if (!tokens || tokens.output <= 0) {
    return undefined;
  }
  return tokens.input + tokens.output + tokens.reasoning + tokens.cache.read + tokens.cache.write;
}

function lastAssistantMessageWithTokens(
  messages: ReadonlyArray<ReporterMessage>,
): ReporterMessage | undefined {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role === "assistant" && messageTokens(message)) {
      return message;
    }
  }
}

function sessionTokens(session: ReporterSession): number | undefined {
  const tokens = session.tokens;
  if (!tokens) {
    return undefined;
  }
  const total =
    tokens.input + tokens.output + tokens.reasoning + tokens.cache.read + tokens.cache.write;
  return total > 0 ? total : undefined;
}

function errorSignature(error: unknown): string {
  return JSON.stringify(errorMetadata(error));
}

function errorMetadata(error: unknown): Record<string, unknown> {
  if (error instanceof Error) {
    switch (error.name) {
      case "AbortError":
        return { category: "aborted" };
      case "TimeoutError":
        return { category: "timeout" };
      case "TypeError":
        return { category: "type-error" };
      case "RangeError":
        return { category: "range-error" };
      default:
        return { category: "error" };
    }
  }
  return { category: typeof error };
}

async function settleWithin(promise: Promise<unknown>, timeoutMs: number): Promise<boolean> {
  return (await settleValueWithin(promise, timeoutMs)).completed;
}

async function settleValueWithin<T>(
  promise: Promise<T>,
  timeoutMs: number,
): Promise<{ completed: true; value: T } | { completed: false }> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<{ completed: false }>((resolve) => {
    timer = setTimeout(() => resolve({ completed: false }), Math.max(0, timeoutMs));
  });
  const result = await Promise.race([
    promise.then((value) => ({ completed: true as const, value })),
    timeout,
  ]);
  if (timer) {
    clearTimeout(timer);
  }
  return result;
}
