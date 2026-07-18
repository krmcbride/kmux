import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";

import plugin from "./kmux-status-server";

afterEach(() => {
  mock.restore();
});

describe("kmux status server adapter", () => {
  test("scopes bootstrap requests to the plugin directory and preserves the session bound", async () => {
    let sessionListOptions: unknown;
    let statusOptions: unknown;
    const client = {
      session: {
        async list(options: unknown) {
          sessionListOptions = options;
          return { data: [] };
        },
        async status(options: unknown) {
          statusOptions = options;
          return { data: {} };
        },
        async messages() {
          return { data: [] };
        },
      },
      config: {
        async providers() {
          return { data: { providers: [] } };
        },
      },
      app: {
        async log() {
          return { data: true };
        },
      },
    };

    const hooks = await plugin.server({
      client,
      directory: "/repo/project-alpha",
      worktree: "/repo/project-alpha",
      project: {},
      experimental_workspace: { register() {} },
      serverUrl: new URL("http://127.0.0.1:4096"),
      $: undefined,
    } as never);
    for (let index = 0; index < 10; index += 1) {
      await Promise.resolve();
    }

    expect(sessionListOptions).toEqual({
      query: { directory: "/repo/project-alpha", limit: 200 },
      signal: expect.any(AbortSignal),
    });
    expect(statusOptions).toEqual({
      query: { directory: "/repo/project-alpha" },
      signal: expect.any(AbortSignal),
    });
    expect(hooks.event).toBeFunction();
    expect(hooks.dispose).toBeFunction();
    await hooks.dispose?.();
  });

  test("sanitizes and scopes the reporter identity used by delete commands", async () => {
    const commands: string[][] = [];
    spyOn(Bun, "spawn").mockImplementation((input) => {
      commands.push([...(input as { cmd: string[] }).cmd]);
      return {
        exited: Promise.resolve(0),
        kill() {},
      } as never;
    });
    const client = {
      session: {
        async list() {
          return { data: [] };
        },
        async status() {
          return { data: {} };
        },
        async messages() {
          return { data: [] };
        },
      },
      config: {
        async providers() {
          return { data: { providers: [] } };
        },
      },
      app: {
        async log() {
          return { data: true };
        },
      },
    };
    const hooks = await plugin.server({
      client,
      directory: "/repo/project-alpha",
      worktree: "/repo/project-alpha",
      project: {},
      experimental_workspace: { register() {} },
      serverUrl: new URL("http://user:secret@127.0.0.1:4096/"),
      $: undefined,
    } as never);
    for (let index = 0; index < 10; index += 1) {
      await Promise.resolve();
    }

    await hooks.event?.({
      event: {
        type: "session.created",
        properties: {
          info: {
            id: "root",
            directory: "/repo/project-alpha",
            title: "Example session",
          },
        },
      },
    } as never);
    await hooks.event?.({
      event: {
        type: "session.deleted",
        properties: { info: { id: "root" } },
      },
    } as never);
    for (let index = 0; index < 20; index += 1) {
      await Promise.resolve();
    }

    expect(commands.at(-1)).toEqual([
      "kmux",
      "set-agent-status",
      "--agent-kind",
      "opencode",
      "--session-id",
      "root",
      "--reporter-kind",
      "server",
      "--reporter-instance",
      '["http://127.0.0.1:4096","/repo/project-alpha"]',
      "--delete-session",
    ]);
    await hooks.dispose?.();
  });
});
