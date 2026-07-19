#!/usr/bin/env bun

/** Existing-server OpenCode launcher used by kmux launcher profiles. */

import { realpath } from "node:fs/promises";

const DEFAULT_USERNAME = "opencode";

type AttachProcess = {
  exited: Promise<number>;
  kill(signal: NodeJS.Signals): void;
};

type SpawnOptions = {
  stdin: "inherit";
  stdout: "inherit";
  stderr: "inherit";
};

type FetchRequest = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

type ForwardedSignal = "SIGINT" | "SIGHUP" | "SIGTERM";

export type LauncherRuntime = {
  fetch: FetchRequest;
  cwd(): string;
  realpath(path: string): Promise<string>;
  env: Readonly<Record<string, string | undefined>>;
  spawn(command: string[], options: SpawnOptions): AttachProcess;
  onSignal(signal: ForwardedSignal, listener: () => void): () => void;
  writeError(message: string): void;
};

type LauncherInput = {
  serverUrl: URL;
  attachUrl: string;
  prompt?: string;
};

type SessionResponse = {
  id: string;
  directory: string;
};

class LauncherError extends Error {}

const defaultRuntime: LauncherRuntime = {
  fetch: (input, init) => globalThis.fetch(input, init),
  cwd: () => process.cwd(),
  realpath,
  env: process.env,
  spawn(command, options) {
    return Bun.spawn({
      cmd: command,
      stdin: options.stdin,
      stdout: options.stdout,
      stderr: options.stderr,
    });
  },
  onSignal(signal, listener) {
    process.on(signal, listener);
    return () => process.off(signal, listener);
  },
  writeError(message) {
    console.error(message);
  },
};

function parseInput(argv: readonly string[]): LauncherInput {
  if (argv[0] !== "--server-url" || argv[1] === undefined || argv[1].length === 0) {
    throw new LauncherError("usage: kmux-opencode-launcher --server-url <URL> [PROMPT]");
  }
  if (argv.length > 3) {
    throw new LauncherError("expected at most one prompt argument");
  }

  let serverUrl: URL;
  try {
    serverUrl = new URL(argv[1]);
  } catch {
    throw new LauncherError("server URL is invalid");
  }
  if (serverUrl.protocol !== "http:" && serverUrl.protocol !== "https:") {
    throw new LauncherError("server URL must use http or https");
  }
  if (serverUrl.username.length > 0 || serverUrl.password.length > 0) {
    throw new LauncherError("server URL must not contain credentials");
  }
  if (serverUrl.pathname !== "/" || serverUrl.search.length > 0 || serverUrl.hash.length > 0) {
    throw new LauncherError("server URL must contain only an origin");
  }

  return {
    serverUrl,
    attachUrl: serverUrl.toString().replace(/\/$/, ""),
    ...(argv.length === 3 ? { prompt: argv[2] } : {}),
  };
}

function requestHeaders(env: Readonly<Record<string, string | undefined>>): Headers {
  const headers = new Headers({ "content-type": "application/json" });
  const password = env.OPENCODE_SERVER_PASSWORD;
  if (password !== undefined && password.length > 0) {
    const username = env.OPENCODE_SERVER_USERNAME || DEFAULT_USERNAME;
    headers.set(
      "authorization",
      `Basic ${Buffer.from(`${username}:${password}`, "utf8").toString("base64")}`,
    );
  }
  return headers;
}

function scopedUrl(serverUrl: URL, path: string, directory: string): URL {
  const url = new URL(path, serverUrl);
  url.searchParams.set("directory", directory);
  return url;
}

async function fetchResponse(
  runtime: LauncherRuntime,
  operation: string,
  url: URL,
  init: RequestInit,
): Promise<Response> {
  try {
    return await runtime.fetch(url, init);
  } catch {
    throw new LauncherError(`OpenCode ${operation} request failed`);
  }
}

async function responseJson(response: Response, operation: string): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    throw new LauncherError(`OpenCode ${operation} returned an invalid response`);
  }
}

function validateSession(value: unknown, directory: string): SessionResponse {
  if (typeof value !== "object" || value === null) {
    throw new LauncherError("OpenCode session create returned an invalid response");
  }
  const candidate = value as { id?: unknown; directory?: unknown };
  if (typeof candidate.id !== "string" || candidate.id.trim().length === 0) {
    throw new LauncherError("OpenCode session create returned an invalid response");
  }
  if (candidate.directory !== directory) {
    throw new LauncherError("OpenCode session create returned a different directory");
  }
  return { id: candidate.id, directory };
}

async function createSession(
  runtime: LauncherRuntime,
  input: LauncherInput,
  directory: string,
  headers: Headers,
): Promise<SessionResponse> {
  const response = await fetchResponse(
    runtime,
    "session create",
    scopedUrl(input.serverUrl, "/session", directory),
    { method: "POST", headers, body: "{}", redirect: "error" },
  );
  if (response.status !== 200) {
    throw new LauncherError(`OpenCode session create failed with HTTP ${response.status}`);
  }
  return validateSession(await responseJson(response, "session create"), directory);
}

async function submitPrompt(
  runtime: LauncherRuntime,
  input: LauncherInput,
  directory: string,
  session: SessionResponse,
  headers: Headers,
): Promise<void> {
  const response = await fetchResponse(
    runtime,
    "prompt submit",
    scopedUrl(
      input.serverUrl,
      `/session/${encodeURIComponent(session.id)}/prompt_async`,
      directory,
    ),
    {
      method: "POST",
      headers,
      body: JSON.stringify({ parts: [{ type: "text", text: input.prompt }] }),
      redirect: "error",
    },
  );
  if (response.status !== 204) {
    throw new LauncherError(`OpenCode prompt submit failed with HTTP ${response.status}`);
  }
}

async function canonicalDirectory(runtime: LauncherRuntime): Promise<string> {
  try {
    return await runtime.realpath(runtime.cwd());
  } catch {
    throw new LauncherError("could not resolve the launcher working directory");
  }
}

const SIGNAL_EXIT_CODES: Readonly<Record<ForwardedSignal, number>> = {
  SIGHUP: 129,
  SIGINT: 130,
  SIGTERM: 143,
};

async function waitForAttach(runtime: LauncherRuntime, child: AttachProcess): Promise<number> {
  let receivedSignal: ForwardedSignal | undefined;
  const removeListeners = (Object.keys(SIGNAL_EXIT_CODES) as ForwardedSignal[]).map((signal) =>
    runtime.onSignal(signal, () => {
      receivedSignal ??= signal;
      try {
        child.kill(signal);
      } catch {
        // Continue waiting so a failed forwarding attempt cannot orphan an unreaped child.
      }
    }),
  );

  try {
    const exitCode = await child.exited;
    return receivedSignal === undefined ? exitCode : SIGNAL_EXIT_CODES[receivedSignal];
  } finally {
    for (const removeListener of removeListeners) {
      removeListener();
    }
  }
}

/** Run one existing-server OpenCode launch and return the attached TUI's exit status. */
export async function runLauncher(
  argv: readonly string[],
  runtime: LauncherRuntime = defaultRuntime,
): Promise<number> {
  const input = parseInput(argv);
  const directory = await canonicalDirectory(runtime);
  const attachCommand = ["opencode", "attach", input.attachUrl, "--dir", directory];

  if (input.prompt !== undefined) {
    const headers = requestHeaders(runtime.env);
    const session = await createSession(runtime, input, directory, headers);
    await submitPrompt(runtime, input, directory, session, headers);
    attachCommand.push("--session", session.id);
  }

  let child: AttachProcess;
  try {
    child = runtime.spawn(attachCommand, {
      stdin: "inherit",
      stdout: "inherit",
      stderr: "inherit",
    });
  } catch {
    throw new LauncherError("could not start the OpenCode attach client");
  }
  return await waitForAttach(runtime, child);
}

/** CLI boundary that emits only fixed, prompt-free diagnostics. */
export async function main(
  argv: readonly string[],
  runtime: LauncherRuntime = defaultRuntime,
): Promise<number> {
  try {
    return await runLauncher(argv, runtime);
  } catch (error) {
    const message = error instanceof LauncherError ? error.message : "unexpected launcher failure";
    runtime.writeError(`kmux-opencode-launcher: ${message}`);
    return 1;
  }
}

if (import.meta.main) {
  process.exitCode = await main(process.argv.slice(2));
}
