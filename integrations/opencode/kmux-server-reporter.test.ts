import { describe, expect, test } from "bun:test";

import {
  KmuxServerReporter,
  type KmuxServerReporterDependencies,
  type ReporterLogEntry,
  type ReporterScheduler,
  type ReporterSource,
} from "./kmux-server-reporter";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (error: unknown) => void = () => {};
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

class ManualScheduler implements ReporterScheduler {
  private tasks: Array<{ cancelled: boolean; task: () => void }> = [];
  throwNext = false;

  schedule(_delayMs: number, task: () => void): () => void {
    if (this.throwNext) {
      this.throwNext = false;
      throw new Error("scheduler unavailable");
    }
    const scheduled = { cancelled: false, task };
    this.tasks.push(scheduled);
    return () => {
      scheduled.cancelled = true;
    };
  }

  flush() {
    while (this.tasks.length > 0) {
      const tasks = this.tasks;
      this.tasks = [];
      for (const task of tasks) {
        if (!task.cancelled) {
          task.task();
        }
      }
    }
  }
}

const DEFAULT_REPORTER_INSTANCE = '["http://127.0.0.1:4096","/repo/project-alpha"]';

type HarnessOptions = {
  source?: Partial<ReporterSource>;
  runCommand?: KmuxServerReporterDependencies["runCommand"];
  reporterInstance?: string;
  bootstrapTimeoutMs?: number;
  contextTimeoutMs?: number;
  disposeTimeoutMs?: number;
};

function createHarness(options: HarnessOptions = {}) {
  const commands: string[][] = [];
  const logs: ReporterLogEntry[] = [];
  const scheduler = new ManualScheduler();
  const source: ReporterSource = {
    sessions: async () => [],
    statuses: async () => ({}),
    messages: async () => [],
    modelLimits: async () => new Map(),
    ...options.source,
  };
  const reporter = new KmuxServerReporter({
    reporterInstance: options.reporterInstance ?? DEFAULT_REPORTER_INSTANCE,
    fallbackDirectory: "/repo/project-alpha",
    source,
    runCommand: async (command) => {
      commands.push([...command]);
      return options.runCommand ? options.runCommand(command) : 0;
    },
    log: (entry) => logs.push(entry),
    scheduler,
    reportDebounceMs: 0,
    bootstrapTimeoutMs: options.bootstrapTimeoutMs ?? 1000,
    contextTimeoutMs: options.contextTimeoutMs ?? 1000,
    disposeTimeoutMs: options.disposeTimeoutMs ?? 1000,
  });
  return { reporter, commands, logs, scheduler };
}

async function settle() {
  for (let index = 0; index < 20; index += 1) {
    await Promise.resolve();
  }
}

function created(id: string, parentID?: string) {
  return {
    type: "session.created",
    properties: {
      info: {
        id,
        parentID,
        directory: "/repo/project-alpha",
        title: `Session ${id}`,
      },
    },
  };
}

function status(sessionID: string, type: "busy" | "idle" | "retry") {
  return {
    type: "session.status",
    properties: { sessionID, status: { type } },
  };
}

function commandStatus(command: ReadonlyArray<string>): string | undefined {
  return command[2]?.startsWith("--") ? undefined : command[2];
}

function expectReporterIdentity(
  command: ReadonlyArray<string>,
  reporterInstance = DEFAULT_REPORTER_INSTANCE,
) {
  const reporterKindIndex = command.indexOf("--reporter-kind");
  expect(command.slice(reporterKindIndex, reporterKindIndex + 4)).toEqual([
    "--reporter-kind",
    "server",
    "--reporter-instance",
    reporterInstance,
  ]);
  expect(command).not.toContain("--producer-kind");
  expect(command).not.toContain("--producer-instance");
}

describe("KmuxServerReporter", () => {
  test("emits the server reporter identity", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    const command = harness.commands.at(-1) ?? [];
    expectReporterIdentity(command);
  });

  test("buffers events during bootstrap so newer hook state wins", async () => {
    const sessions = deferred<ReadonlyArray<{ id: string; directory: string }>>();
    const statuses = deferred<Readonly<Record<string, unknown>>>();
    const harness = createHarness({
      source: {
        sessions: () => sessions.promise,
        statuses: () => statuses.promise,
      },
    });

    const startup = harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    sessions.resolve([{ id: "root", directory: "/repo/project-alpha" }]);
    statuses.resolve({ root: { type: "idle" } });
    await startup;
    harness.scheduler.flush();
    await settle();

    expect(harness.commands.at(-1)).toContain("working");
    expect(harness.commands.at(-1)).toContain("--directory");
  });

  test("times out bootstrap once, replays buffered events, and continues reporting", async () => {
    const neverSessions = new Promise<ReadonlyArray<never>>(() => {});
    const neverStatuses = new Promise<Readonly<Record<string, unknown>>>(() => {});
    const harness = createHarness({
      source: {
        sessions: () => neverSessions,
        statuses: () => neverStatuses,
      },
      bootstrapTimeoutMs: 0,
    });

    const startup = harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    await startup;
    harness.scheduler.flush();
    await settle();

    expect(harness.logs.filter((entry) => entry.message.includes("bootstrap"))).toHaveLength(1);
    expect(harness.logs[0]?.message).toContain("timed out");
    expect(harness.commands.at(-1)).toContain("working");
  });

  test("aggregates arbitrary-depth descendants by status priority and tolerates cycles", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(created("child", "root"));
    await harness.reporter.event(created("grandchild", "child"));
    await harness.reporter.event(status("grandchild", "busy"));
    await harness.reporter.event({ type: "permission.asked", properties: { sessionID: "child" } });
    harness.scheduler.flush();
    await settle();

    expect(commandStatus(harness.commands.at(-1) ?? [])).toBe("waiting");

    await harness.reporter.event({
      type: "session.updated",
      properties: {
        info: {
          id: "root",
          parentID: "grandchild",
          directory: "/repo/project-alpha",
        },
      },
    });
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.length).toBeGreaterThan(0);
  });

  test("cleans the old root before reporting a late parent", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("child"));
    await harness.reporter.event(status("child", "busy"));
    harness.scheduler.flush();
    await settle();

    await harness.reporter.event({
      type: "session.updated",
      properties: {
        info: {
          id: "child",
          parentID: "root",
          directory: "/repo/project-alpha",
        },
      },
    });
    await harness.reporter.event(created("root"));
    harness.scheduler.flush();
    await settle();

    const childClear = harness.commands.find(
      (command) => command.includes("child") && command.includes("--delete"),
    );
    const rootSet = harness.commands.find(
      (command) => command.includes("root") && command.includes("working"),
    );
    expect(childClear).toBeDefined();
    expect(rootSet).toBeDefined();
    expect(harness.commands.indexOf(childClear ?? [])).toBeLessThan(
      harness.commands.indexOf(rootSet ?? []),
    );
  });

  test("recomputes after child deletion and deletes all reporters for a deleted root", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(created("child", "root"));
    await harness.reporter.event(status("root", "busy"));
    await harness.reporter.event({ type: "question.asked", properties: { sessionID: "child" } });
    harness.scheduler.flush();
    await settle();
    expect(commandStatus(harness.commands.at(-1) ?? [])).toBe("waiting");

    await harness.reporter.event({
      type: "session.deleted",
      properties: { info: { id: "child", parentID: "root" } },
    });
    harness.scheduler.flush();
    await settle();
    expect(commandStatus(harness.commands.at(-1) ?? [])).toBe("working");

    await harness.reporter.event({
      type: "session.deleted",
      properties: { info: { id: "root" } },
    });
    await settle();
    const deleteCommand = harness.commands.at(-1) ?? [];
    expect(deleteCommand).toContain("--delete-session");
    expectReporterIdentity(deleteCommand);
  });

  test("prevents stale context work from overwriting a newer report revision", async () => {
    const firstMessages =
      deferred<
        ReadonlyArray<{
          role: string;
          providerID: string;
          modelID: string;
          tokens: {
            input: number;
            output: number;
            reasoning: number;
            cache: { read: number; write: number };
          };
        }>
      >();
    let messageCalls = 0;
    const harness = createHarness({
      source: {
        messages: () => {
          messageCalls += 1;
          return messageCalls === 1 ? firstMessages.promise : Promise.resolve([]);
        },
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    await harness.reporter.event({ type: "permission.asked", properties: { sessionID: "root" } });
    harness.scheduler.flush();
    await settle();
    firstMessages.resolve([
      {
        role: "assistant",
        providerID: "provider-alpha",
        modelID: "model-alpha",
        tokens: { input: 90, output: 10, reasoning: 0, cache: { read: 0, write: 0 } },
      },
    ]);
    await settle();

    expect(harness.commands).toHaveLength(1);
    expect(commandStatus(harness.commands[0] ?? [])).toBe("waiting");
  });

  test("bounds hanging message enrichment without delaying the base report indefinitely", async () => {
    const never = new Promise<ReadonlyArray<never>>(() => {});
    const harness = createHarness({
      source: { messages: () => never },
      contextTimeoutMs: 0,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await new Promise((resolve) => setTimeout(resolve, 1));
    await settle();

    expect(harness.commands.at(-1)).toContain("working");
    expect(harness.logs.some((entry) => entry.message.includes("enrichment timed out"))).toBe(true);
  });

  test("bounds hanging model-limit enrichment without suppressing the base report", async () => {
    const never = new Promise<ReadonlyMap<string, number>>(() => {});
    const harness = createHarness({
      source: {
        messages: async () => [
          {
            role: "assistant",
            providerID: "provider-alpha",
            modelID: "model-alpha",
            tokens: { input: 90, output: 10, reasoning: 0, cache: { read: 0, write: 0 } },
          },
        ],
        modelLimits: () => never,
      },
      contextTimeoutMs: 0,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await new Promise((resolve) => setTimeout(resolve, 1));
    await settle();

    expect(harness.commands.at(-1)).toContain("working");
    expect(harness.commands.at(-1)).not.toContain("100");
  });

  test("retries model limits after a timed-out source ignores cancellation", async () => {
    const never = new Promise<ReadonlyMap<string, number>>(() => {});
    let modelCalls = 0;
    const harness = createHarness({
      source: {
        messages: async () => [
          {
            role: "assistant",
            providerID: "provider-alpha",
            modelID: "model-alpha",
            tokens: { input: 90, output: 10, reasoning: 0, cache: { read: 0, write: 0 } },
          },
        ],
        modelLimits: () => {
          modelCalls += 1;
          return modelCalls === 1
            ? never
            : Promise.resolve(new Map([["provider-alpha/model-alpha", 1000]]));
        },
      },
      contextTimeoutMs: 5,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await new Promise((resolve) => setTimeout(resolve, 10));
    await settle();
    expect(harness.commands.at(-1)).not.toContain("100");

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.at(-1)).toContain("100 (10%)");
    expect(modelCalls).toBe(2);
  });

  test("does not let an older model-limit request overwrite a newer cached value", async () => {
    const olderLimits = deferred<ReadonlyMap<string, number>>();
    let modelCalls = 0;
    const harness = createHarness({
      source: {
        messages: async () => [
          {
            role: "assistant",
            providerID: "provider-alpha",
            modelID: "model-alpha",
            tokens: { input: 90, output: 10, reasoning: 0, cache: { read: 0, write: 0 } },
          },
        ],
        modelLimits: () => {
          modelCalls += 1;
          return modelCalls === 1
            ? olderLimits.promise
            : Promise.resolve(new Map([["provider-alpha/model-alpha", 1000]]));
        },
      },
      contextTimeoutMs: 5,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await new Promise((resolve) => setTimeout(resolve, 10));
    await settle();

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.at(-1)).toContain("100 (10%)");
    olderLimits.resolve(new Map([["provider-alpha/model-alpha", 100]]));
    await settle();

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands).toHaveLength(2);
    expect(harness.commands.at(-1)).toContain("100 (10%)");
    expect(modelCalls).toBe(2);
  });

  test("does not add message info objects to session topology", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    await harness.reporter.event({
      type: "message.updated",
      properties: {
        info: {
          id: "message-alpha",
          sessionID: "root",
          role: "assistant",
        },
      },
    });
    harness.scheduler.flush();
    await settle();
    await harness.reporter.event(status("message-alpha", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(harness.commands).toHaveLength(1);
    expect(harness.commands[0]).toContain("root");
    expect(harness.commands[0]).not.toContain("message-alpha");
  });

  test("keeps base reporting available when context lookups fail and retries model limits", async () => {
    let modelCalls = 0;
    const harness = createHarness({
      source: {
        messages: async () => [
          {
            role: "assistant",
            providerID: "provider-alpha",
            modelID: "model-alpha",
            tokens: { input: 90, output: 10, reasoning: 0, cache: { read: 0, write: 0 } },
          },
        ],
        modelLimits: async () => {
          modelCalls += 1;
          if (modelCalls === 1) {
            throw new Error("provider temporarily unavailable");
          }
          return new Map([["provider-alpha/model-alpha", 1000]]);
        },
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.at(-1)).toContain("100");

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.at(-1)).toContain("100 (10%)");
    expect(modelCalls).toBe(2);
  });

  test("keeps base reporting available when message lookup fails", async () => {
    const harness = createHarness({
      source: {
        messages: async () => {
          throw new Error("messages temporarily unavailable");
        },
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(harness.commands.at(-1)).toContain("working");
    expect(harness.logs.some((entry) => entry.message.includes("messages unavailable"))).toBe(true);
  });

  test("logs bootstrap and event failures without disabling later events", async () => {
    const harness = createHarness({
      source: {
        sessions: async () => {
          throw new Error("session snapshot unavailable");
        },
        statuses: async () => {
          throw new Error("status snapshot unavailable");
        },
      },
    });
    await harness.reporter.start();
    harness.scheduler.throwNext = true;
    await expect(harness.reporter.event(status("root", "busy"))).resolves.toBeUndefined();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(harness.logs.some((entry) => entry.message.includes("bootstrap failed"))).toBe(true);
    expect(harness.logs.some((entry) => entry.message.includes("event handling failed"))).toBe(
      true,
    );
    expect(harness.commands.at(-1)).toContain("working");
  });

  test("never includes arbitrary error messages in structured diagnostics", async () => {
    const secret = "token-and-private-directory-value";
    let commandCalls = 0;
    const harness = createHarness({
      source: {
        sessions: async () => {
          throw new Error(secret);
        },
      },
      runCommand: async () => {
        commandCalls += 1;
        if (commandCalls === 1) {
          throw new Error(secret);
        }
        return 0;
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(JSON.stringify(harness.logs)).not.toContain(secret);
    expect(JSON.stringify(harness.logs)).toContain('"category":"error"');
  });

  test("handles versioned permission and question transitions", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event({
      type: "permission.v2.asked",
      properties: { sessionID: "root" },
    });
    harness.scheduler.flush();
    await settle();
    expect(commandStatus(harness.commands.at(-1) ?? [])).toBe("waiting");

    await harness.reporter.event({
      type: "question.v2.replied",
      properties: { sessionID: "root" },
    });
    harness.scheduler.flush();
    await settle();
    expect(commandStatus(harness.commands.at(-1) ?? [])).toBe("working");
  });

  test("retries failed reports, deduplicates pending reports, and protects newer delivery state", async () => {
    const first = deferred<number>();
    const second = deferred<number>();
    let calls = 0;
    const harness = createHarness({
      runCommand: () => {
        calls += 1;
        return calls === 1 ? first.promise : second.promise;
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands).toHaveLength(1);

    await harness.reporter.event({ type: "permission.asked", properties: { sessionID: "root" } });
    harness.scheduler.flush();
    await settle();
    first.resolve(1);
    await settle();
    expect(harness.commands).toHaveLength(2);

    await harness.reporter.event({ type: "permission.asked", properties: { sessionID: "root" } });
    harness.scheduler.flush();
    await settle();
    expect(harness.commands).toHaveLength(2);
    second.resolve(0);
    await settle();

    await harness.reporter.event({ type: "permission.asked", properties: { sessionID: "root" } });
    harness.scheduler.flush();
    await settle();
    expect(harness.commands).toHaveLength(2);
    expect(harness.logs.some((entry) => entry.message.includes("delivery failed"))).toBe(true);
  });

  test("retries an unchanged report after a non-zero delivery", async () => {
    const results = [1, 0];
    const harness = createHarness({
      runCommand: async () => results.shift() ?? 0,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(harness.commands).toHaveLength(2);
    expect(harness.logs.some((entry) => entry.message.includes("recovered"))).toBe(true);
  });

  test("retries an unchanged report after a command runner error", async () => {
    let calls = 0;
    const harness = createHarness({
      runCommand: async () => {
        calls += 1;
        if (calls === 1) {
          throw new Error("spawn failed");
        }
        return 0;
      },
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    expect(harness.commands).toHaveLength(2);
    expect(harness.logs.some((entry) => entry.message.includes("delivery failed"))).toBe(true);
  });

  test("cleans managed roots on idempotent disposal", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    const firstDispose = harness.reporter.dispose();
    const secondDispose = harness.reporter.dispose();
    expect(firstDispose).toBe(secondDispose);
    await firstDispose;
    const deleteCommand = harness.commands.at(-1) ?? [];
    expect(deleteCommand).toContain("--delete");
    expectReporterIdentity(deleteCommand);

    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    expect(harness.commands.at(-1)).toContain("--delete");
  });

  test("cancels reports that have not reached the command queue during disposal", async () => {
    const harness = createHarness();
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));

    await harness.reporter.dispose();
    harness.scheduler.flush();
    await settle();
    expect(harness.commands).toHaveLength(0);
  });

  test("disposal cleans a set queued behind an older clear", async () => {
    const firstClear = deferred<number>();
    const queuedSet = deferred<number>();
    let commandCalls = 0;
    const harness = createHarness({
      source: {
        sessions: async () => [{ id: "root", directory: "/repo/project-alpha" }],
        statuses: async () => ({ root: { type: "idle" } }),
      },
      runCommand: () => {
        commandCalls += 1;
        if (commandCalls === 1) {
          return firstClear.promise;
        }
        if (commandCalls === 2) {
          return queuedSet.promise;
        }
        return Promise.resolve(0);
      },
      disposeTimeoutMs: 0,
    });
    await harness.reporter.start();
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();
    firstClear.resolve(0);
    await settle();
    expect(harness.commands).toHaveLength(2);

    await harness.reporter.dispose();
    queuedSet.resolve(0);
    await settle();
    expect(harness.commands).toHaveLength(3);
    expect(harness.commands.at(-1)).toContain("--delete");
  });

  test("keeps directory-scoped reporter cleanup isolated", async () => {
    const firstReporterInstance = '["http://127.0.0.1:4096","/repo/project-alpha"]';
    const secondReporterInstance = '["http://127.0.0.1:4096","/repo/project-beta"]';
    const first = createHarness({ reporterInstance: firstReporterInstance });
    const second = createHarness({ reporterInstance: secondReporterInstance });
    await Promise.all([first.reporter.start(), second.reporter.start()]);
    await first.reporter.event(created("root-alpha"));
    await second.reporter.event(created("root-beta"));
    await first.reporter.event(status("root-alpha", "busy"));
    await second.reporter.event(status("root-beta", "busy"));
    first.scheduler.flush();
    second.scheduler.flush();
    await settle();

    await first.reporter.dispose();
    expect(first.commands.at(-1)).toContain(firstReporterInstance);
    expect(first.commands.at(-1)).toContain("--delete");
    expect(second.commands).toHaveLength(1);
    expect(second.commands[0]).toContain(secondReporterInstance);
  });

  test("bounds disposal when a command never exits", async () => {
    const never = new Promise<number>(() => {});
    const harness = createHarness({
      runCommand: () => never,
      disposeTimeoutMs: 0,
    });
    await harness.reporter.start();
    await harness.reporter.event(created("root"));
    await harness.reporter.event(status("root", "busy"));
    harness.scheduler.flush();
    await settle();

    await harness.reporter.dispose();
    expect(harness.logs.some((entry) => entry.message.includes("Timed out draining"))).toBe(true);
  });
});
