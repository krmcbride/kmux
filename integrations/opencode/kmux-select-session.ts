#!/usr/bin/env bun

/**
 * kmux sidebar selection hook for OpenCode.
 *
 * This consumes kmux's generic sidebar hook payload and asks an OpenCode server
 * to navigate attached TUIs to the selected session.
 */

const AGENT_KIND = "opencode";
const SERVER_PRODUCER_KIND = "server";
const DEFAULT_TIMEOUT_MS = 1000;

type HookPayload = {
  agent?: Record<string, unknown>;
  target?: Record<string, unknown>;
  observations?: unknown[];
};

function clean(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed || undefined;
}

function optionalRecord(value: unknown): Record<string, unknown> | undefined {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  return undefined;
}

function parsePayload(input: string): HookPayload {
  const parsed: unknown = JSON.parse(input);
  const payload = optionalRecord(parsed);
  if (!payload) throw new Error("kmux hook payload must be a JSON object");
  return payload;
}

function agentKind(payload: HookPayload): string | undefined {
  return clean(optionalRecord(payload.agent)?.kind);
}

function sessionID(payload: HookPayload): string | undefined {
  return clean(optionalRecord(payload.agent)?.session_id);
}

function selectedDirectory(payload: HookPayload): string | undefined {
  const target = optionalRecord(payload.target);
  return clean(target?.git_worktree_path) ?? clean(target?.directory);
}

function configuredServerUrl(): string | undefined {
  return validHttpUrl(clean(Bun.env.OPENCODE_SERVER_URL));
}

function serverUrlFromPayload(payload: HookPayload): string | undefined {
  for (const item of payload.observations ?? []) {
    const observation = optionalRecord(item);
    if (!observation) continue;
    if (clean(observation.producer_kind) !== SERVER_PRODUCER_KIND) continue;
    const url = validHttpUrl(clean(observation.producer_instance));
    if (url) return url;
  }
  return configuredServerUrl();
}

function validHttpUrl(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    return url.toString();
  } catch {
    return undefined;
  }
}

function selectionEndpoint(
  serverUrl: string,
  directory: string | undefined,
): URL {
  const endpoint = new URL(serverUrl);
  const basePath = endpoint.pathname.replace(/\/$/, "");
  endpoint.pathname = `${basePath}/tui/select-session`;
  endpoint.search = "";
  if (directory) endpoint.searchParams.set("directory", directory);
  return endpoint;
}

function authHeaders(): Record<string, string> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  const password = clean(Bun.env.OPENCODE_SERVER_PASSWORD);
  if (!password) return headers;

  const username = clean(Bun.env.OPENCODE_SERVER_USERNAME) ?? "opencode";
  headers.Authorization = `Basic ${btoa(`${username}:${password}`)}`;
  return headers;
}

function timeoutMs(): number {
  const configured = clean(Bun.env.KMUX_OPENCODE_SELECT_TIMEOUT_MS);
  if (!configured) return DEFAULT_TIMEOUT_MS;
  const parsed = Number(configured);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_TIMEOUT_MS;
}

async function selectSession(
  serverUrl: string,
  sessionID: string,
  directory: string | undefined,
) {
  const endpoint = selectionEndpoint(serverUrl, directory);
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs());
  try {
    const response = await fetch(endpoint, {
      method: "POST",
      headers: authHeaders(),
      body: JSON.stringify({ sessionID }),
      signal: controller.signal,
    });
    if (!response.ok) {
      const body = await response.text().catch(() => "");
      throw new Error(
        `OpenCode select-session failed: HTTP ${response.status} ${body}`.trim(),
      );
    }
  } finally {
    clearTimeout(timeout);
  }
}

async function main() {
  const payload = parsePayload(await Bun.stdin.text());
  if (agentKind(payload) !== AGENT_KIND) return;

  const selectedSessionID = sessionID(payload);
  if (!selectedSessionID)
    throw new Error("kmux hook payload is missing agent.session_id");

  const serverUrl = serverUrlFromPayload(payload);
  if (!serverUrl) {
    console.error(
      "OpenCode select-session skipped: no server producer URL and OPENCODE_SERVER_URL is not set",
    );
    return;
  }

  await selectSession(serverUrl, selectedSessionID, selectedDirectory(payload));
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
