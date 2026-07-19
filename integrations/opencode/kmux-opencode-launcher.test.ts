import { describe, expect, test } from "bun:test";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { type LauncherRuntime, main, runLauncher } from "./kmux-opencode-launcher";

type FetchCall = {
  url: URL;
  init: RequestInit;
};

type RuntimeFixture = {
  runtime: LauncherRuntime;
  fetchCalls: FetchCall[];
  spawnCalls: Array<{ command: string[]; options: unknown }>;
  errors: string[];
  signalListeners: Map<string, Set<() => void>>;
};

function fixture(
  responses: Response[] = [],
  env: Record<string, string | undefined> = {},
): RuntimeFixture {
  const fetchCalls: FetchCall[] = [];
  const spawnCalls: Array<{ command: string[]; options: unknown }> = [];
  const errors: string[] = [];
  const signalListeners = new Map<string, Set<() => void>>();
  return {
    fetchCalls,
    spawnCalls,
    errors,
    signalListeners,
    runtime: {
      async fetch(input, init = {}) {
        fetchCalls.push({ url: new URL(input.toString()), init });
        const response = responses.shift();
        if (response === undefined) {
          throw new Error("unexpected fetch");
        }
        return response;
      },
      cwd: () => "/repo/project-alpha-link",
      async realpath() {
        return "/repo/project-alpha";
      },
      env,
      spawn(command, options) {
        spawnCalls.push({ command, options });
        return { exited: Promise.resolve(0), kill() {} };
      },
      onSignal(signal, listener) {
        const listeners = signalListeners.get(signal) ?? new Set();
        listeners.add(listener);
        signalListeners.set(signal, listeners);
        return () => listeners.delete(listener);
      },
      writeError(message) {
        errors.push(message);
      },
    },
  };
}

function createdSession(directory = "/repo/project-alpha"): Response {
  return Response.json({ id: "ses_project_alpha", directory });
}

describe("kmux OpenCode launcher", () => {
  test("creates, prompts, and attaches to one directory-scoped session", async () => {
    const state = fixture([createdSession(), new Response(null, { status: 204 })]);

    const exitCode = await runLauncher(
      ["--server-url", "http://127.0.0.1:4096/", "Implement the requested phase."],
      state.runtime,
    );

    expect(exitCode).toBe(0);
    expect(state.fetchCalls).toHaveLength(2);
    expect(state.fetchCalls[0]?.url.pathname).toBe("/session");
    expect(state.fetchCalls[0]?.url.searchParams.get("directory")).toBe("/repo/project-alpha");
    expect(state.fetchCalls[0]?.init.method).toBe("POST");
    expect(state.fetchCalls[0]?.init.body).toBe("{}");
    expect(state.fetchCalls[0]?.init.redirect).toBe("error");
    expect(state.fetchCalls[1]?.url.pathname).toBe("/session/ses_project_alpha/prompt_async");
    expect(state.fetchCalls[1]?.url.searchParams.get("directory")).toBe("/repo/project-alpha");
    expect(JSON.parse(String(state.fetchCalls[1]?.init.body))).toEqual({
      parts: [{ type: "text", text: "Implement the requested phase." }],
    });
    expect(state.fetchCalls[1]?.init.redirect).toBe("error");
    expect(state.spawnCalls).toEqual([
      {
        command: [
          "opencode",
          "attach",
          "http://127.0.0.1:4096",
          "--dir",
          "/repo/project-alpha",
          "--session",
          "ses_project_alpha",
        ],
        options: { stdin: "inherit", stdout: "inherit", stderr: "inherit" },
      },
    ]);
  });

  test("attaches without API calls or arbitrary session selection when prompt is absent", async () => {
    const state = fixture();

    await runLauncher(["--server-url", "https://example.invalid"], state.runtime);

    expect(state.fetchCalls).toEqual([]);
    expect(state.spawnCalls[0]?.command).toEqual([
      "opencode",
      "attach",
      "https://example.invalid",
      "--dir",
      "/repo/project-alpha",
    ]);
  });

  test("preserves an empty or hyphen-leading prompt as the final API value", async () => {
    for (const prompt of ["", "--leading-dash", "line one\nline two"]) {
      const state = fixture([createdSession(), new Response(null, { status: 204 })]);

      await runLauncher(["--server-url", "https://example.invalid", prompt], state.runtime);

      expect(JSON.parse(String(state.fetchCalls[1]?.init.body))).toEqual({
        parts: [{ type: "text", text: prompt }],
      });
    }
  });

  test("uses standard Basic auth environment with the default or configured username", async () => {
    for (const [env, expected] of [
      [{ OPENCODE_SERVER_PASSWORD: "example-password" }, "opencode:example-password"],
      [
        {
          OPENCODE_SERVER_PASSWORD: "example-password",
          OPENCODE_SERVER_USERNAME: "example-user",
        },
        "example-user:example-password",
      ],
    ] as const) {
      const state = fixture([createdSession(), new Response(null, { status: 204 })], env);

      await runLauncher(
        ["--server-url", "https://example.invalid", "Example prompt"],
        state.runtime,
      );

      const headers = new Headers(state.fetchCalls[0]?.init.headers);
      expect(headers.get("authorization")).toBe(
        `Basic ${Buffer.from(expected).toString("base64")}`,
      );
    }
  });

  test("does not send authorization when the password is absent or empty", async () => {
    for (const env of [{}, { OPENCODE_SERVER_PASSWORD: "" }]) {
      const state = fixture([createdSession(), new Response(null, { status: 204 })], env);

      await runLauncher(
        ["--server-url", "https://example.invalid", "Example prompt"],
        state.runtime,
      );

      expect(new Headers(state.fetchCalls[0]?.init.headers).has("authorization")).toBeFalse();
    }
  });

  test("URL-encodes directory and session identifiers", async () => {
    const state = fixture([
      Response.json({ id: "ses/example value", directory: "/repo/project alpha" }),
      new Response(null, { status: 204 }),
    ]);
    state.runtime.realpath = async () => "/repo/project alpha";

    await runLauncher(["--server-url", "https://example.invalid", "Example prompt"], state.runtime);

    expect(state.fetchCalls[0]?.url.search).toBe("?directory=%2Frepo%2Fproject+alpha");
    expect(state.fetchCalls[1]?.url.pathname).toBe("/session/ses%2Fexample%20value/prompt_async");
  });

  test("rejects invalid invocation and non-origin server URLs before network access", async () => {
    for (const argv of [
      [],
      ["--server-url"],
      ["--server-url", "not-a-url"],
      ["--server-url", "file:///tmp/socket"],
      ["--server-url", "https://user:secret@example.invalid"],
      ["--server-url", "https://example.invalid/base"],
      ["--server-url", "https://example.invalid?token=secret"],
      ["--server-url", "https://example.invalid#fragment"],
      ["--server-url", "https://example.invalid", "one", "two"],
    ]) {
      const state = fixture();

      expect(await main(argv, state.runtime)).toBe(1);
      expect(state.fetchCalls).toEqual([]);
      expect(state.spawnCalls).toEqual([]);
      expect(state.errors).toHaveLength(1);
    }
  });

  test("rejects malformed session responses and directory mismatches", async () => {
    for (const response of [
      Response.json({}),
      Response.json({ id: "", directory: "/repo/project-alpha" }),
      createdSession("/repo/project-beta"),
      new Response("not json", { status: 200 }),
    ]) {
      const state = fixture([response]);

      expect(
        await main(["--server-url", "https://example.invalid", "Example prompt"], state.runtime),
      ).toBe(1);
      expect(state.spawnCalls).toEqual([]);
      expect(state.errors[0]).not.toContain("/repo/");
    }
  });

  test("leaves partial external sessions intact after prompt or attach failure", async () => {
    const promptFailure = fixture([createdSession(), new Response(null, { status: 503 })]);
    expect(
      await main(
        ["--server-url", "https://example.invalid", "Example prompt"],
        promptFailure.runtime,
      ),
    ).toBe(1);
    expect(promptFailure.fetchCalls).toHaveLength(2);

    const attachFailure = fixture([createdSession(), new Response(null, { status: 204 })]);
    attachFailure.runtime.spawn = () => {
      throw new Error("spawn detail that must not escape");
    };
    expect(
      await main(
        ["--server-url", "https://example.invalid", "Example prompt"],
        attachFailure.runtime,
      ),
    ).toBe(1);
    expect(attachFailure.fetchCalls).toHaveLength(2);
    expect(attachFailure.errors).toEqual([
      "kmux-opencode-launcher: could not start the OpenCode attach client",
    ]);
  });

  test("returns the attached TUI exit status", async () => {
    const state = fixture();
    state.runtime.spawn = () => ({ exited: Promise.resolve(23), kill() {} });

    expect(await runLauncher(["--server-url", "https://example.invalid"], state.runtime)).toBe(23);
  });

  test("forwards catchable termination signals, reaps the child, and removes listeners", async () => {
    const state = fixture();
    let resolveExit = (_code: number) => {};
    const exited = new Promise<number>((resolve) => {
      resolveExit = resolve;
    });
    const forwarded: string[] = [];
    state.runtime.spawn = () => ({
      exited,
      kill(signal) {
        forwarded.push(signal);
        resolveExit(0);
      },
    });

    const running = runLauncher(["--server-url", "https://example.invalid"], state.runtime);
    for (let attempt = 0; attempt < 10; attempt += 1) {
      if ((state.signalListeners.get("SIGTERM")?.size ?? 0) > 0) {
        break;
      }
      await Promise.resolve();
    }
    expect(state.signalListeners.get("SIGTERM")?.size).toBe(1);
    state.signalListeners.get("SIGTERM")?.forEach((listener) => {
      listener();
    });

    expect(await running).toBe(143);
    expect(forwarded).toEqual(["SIGTERM"]);
    expect(
      [...state.signalListeners.values()].every((listeners) => listeners.size === 0),
    ).toBeTrue();
  });

  test("production fetch refuses redirects before a prompt can reach another origin", async () => {
    let targetRequests = 0;
    const target = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch() {
        targetRequests += 1;
        return new Response(null, { status: 204 });
      },
    });
    const redirect = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch() {
        return Response.redirect(target.url.toString(), 307);
      },
    });
    const state = fixture();
    state.runtime.fetch = (input, init) => globalThis.fetch(input, init);

    try {
      expect(
        await main(
          ["--server-url", redirect.url.toString(), "REDIRECT_PROMPT_SENTINEL"],
          state.runtime,
        ),
      ).toBe(1);
      expect(targetRequests).toBe(0);
      expect(state.spawnCalls).toEqual([]);
      expect(state.errors.join("\n")).not.toContain("REDIRECT_PROMPT_SENTINEL");
    } finally {
      await redirect.stop(true);
      await target.stop(true);
    }
  });

  test("direct SIGTERM to the production adapter terminates its attach child", async () => {
    const directory = await mkdtemp(join(tmpdir(), "kmux-launcher-test-"));
    const executable = join(directory, "opencode");
    const pidFile = join(directory, "child.pid");
    await writeFile(
      executable,
      `#!/usr/bin/env bun
await Bun.write(process.env.KMUX_TEST_PID_FILE, String(process.pid));
for (const signal of ["SIGINT", "SIGHUP", "SIGTERM"]) {
  process.on(signal, () => process.exit(0));
}
await new Promise(() => {});
`,
    );
    await chmod(executable, 0o700);

    const child = Bun.spawn({
      cmd: [
        process.execPath,
        join(import.meta.dir, "kmux-opencode-launcher.ts"),
        "--server-url",
        "https://example.invalid",
      ],
      env: {
        ...process.env,
        KMUX_TEST_PID_FILE: pidFile,
        PATH: `${directory}:${process.env.PATH ?? ""}`,
      },
      stdout: "ignore",
      stderr: "ignore",
    });
    let attachPid: number | undefined;

    try {
      for (let attempt = 0; attempt < 100; attempt += 1) {
        try {
          attachPid = Number.parseInt(await readFile(pidFile, "utf8"), 10);
          break;
        } catch {
          await Bun.sleep(20);
        }
      }
      expect(attachPid).toBeNumber();

      child.kill("SIGTERM");
      expect(await child.exited).toBe(143);
      await Bun.sleep(50);

      let attachAlive = true;
      try {
        process.kill(attachPid as number, 0);
      } catch {
        attachAlive = false;
      }
      expect(attachAlive).toBeFalse();
    } finally {
      if (child.exitCode === null) {
        child.kill("SIGKILL");
        await child.exited;
      }
      if (attachPid !== undefined) {
        try {
          process.kill(attachPid, "SIGKILL");
        } catch {
          // The expected path already reaped the attach process.
        }
      }
      await rm(directory, { recursive: true, force: true });
    }
  });

  test("never includes prompt, password, response, URL credentials, or directory sentinels in errors", async () => {
    const sentinels = [
      "PROMPT_SENTINEL",
      "PASSWORD_SENTINEL",
      "RESPONSE_SENTINEL",
      "URL_SECRET_SENTINEL",
      "DIRECTORY_SENTINEL",
    ];
    const state = fixture([new Response("RESPONSE_SENTINEL", { status: 500 })], {
      OPENCODE_SERVER_PASSWORD: "PASSWORD_SENTINEL",
    });
    state.runtime.realpath = async () => "/repo/DIRECTORY_SENTINEL";

    expect(
      await main(["--server-url", "https://example.invalid", "PROMPT_SENTINEL"], state.runtime),
    ).toBe(1);

    const diagnostics = state.errors.join("\n");
    for (const sentinel of sentinels) {
      expect(diagnostics).not.toContain(sentinel);
    }

    const userinfo = fixture();
    await main(
      ["--server-url", "https://user:URL_SECRET_SENTINEL@example.invalid", "PROMPT_SENTINEL"],
      userinfo.runtime,
    );
    for (const sentinel of sentinels) {
      expect(userinfo.errors.join("\n")).not.toContain(sentinel);
    }
  });
});
