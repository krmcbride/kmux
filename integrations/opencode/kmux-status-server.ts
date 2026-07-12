/** OpenCode server adapter for directory-scoped kmux status reporting. */

import type { Plugin } from "@opencode-ai/plugin";
import type { Message, Provider, Session } from "@opencode-ai/sdk";

import { waitForKmuxChild } from "./kmux-child-process";
import {
  KmuxServerReporter,
  type ReporterLogger,
  type ReporterMessage,
  type ReporterSession,
  type ReporterSource,
  type TokenUsage,
} from "./kmux-server-reporter";

type PluginInput = Parameters<Plugin>[0];
type OpenCodeClient = PluginInput["client"];

declare const Bun: {
  spawn(input: { cmd: string[]; stdout?: "ignore"; stderr?: "ignore" }): {
    exited: Promise<number>;
    kill(signal?: number): void;
  };
};

const COMMAND_TIMEOUT_MS = 2000;
const SESSION_BOOTSTRAP_LIMIT = 200;

function runKmux(command: ReadonlyArray<string>): Promise<number> {
  const child = Bun.spawn({
    cmd: [...command],
    stdout: "ignore",
    stderr: "ignore",
  });
  return waitForKmuxChild(child, COMMAND_TIMEOUT_MS);
}

function responseData<T>(response: unknown, operation: string): T {
  if (response && typeof response === "object" && "data" in response) {
    const data = (response as { data?: T }).data;
    if (data !== undefined) {
      return data;
    }
  }
  throw new Error(`OpenCode ${operation} returned no data`);
}

function clean(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function tokenUsage(value: unknown): TokenUsage | undefined {
  if (typeof value !== "object" || value === null) {
    return undefined;
  }
  const candidate = value as Partial<TokenUsage>;
  if (
    typeof candidate.input !== "number" ||
    typeof candidate.output !== "number" ||
    typeof candidate.reasoning !== "number" ||
    typeof candidate.cache !== "object" ||
    candidate.cache === null ||
    typeof candidate.cache.read !== "number" ||
    typeof candidate.cache.write !== "number"
  ) {
    return undefined;
  }
  return {
    input: candidate.input,
    output: candidate.output,
    reasoning: candidate.reasoning,
    cache: {
      read: candidate.cache.read,
      write: candidate.cache.write,
    },
  };
}

function reporterSession(session: Session): ReporterSession {
  const extended = session as Session & { slug?: unknown; tokens?: unknown };
  return {
    id: session.id,
    parentID: clean(session.parentID),
    directory: clean(session.directory),
    title: clean(session.title),
    slug: clean(extended.slug),
    tokens: tokenUsage(extended.tokens),
  };
}

function reporterMessage(message: Message): ReporterMessage {
  const extended = message as Message & {
    providerID?: unknown;
    modelID?: unknown;
    tokens?: unknown;
  };
  return {
    role: message.role,
    providerID: clean(extended.providerID),
    modelID: clean(extended.modelID),
    tokens: tokenUsage(extended.tokens),
  };
}

function createReporterSource(client: OpenCodeClient, pluginDirectory: string): ReporterSource {
  return {
    async sessions(signal) {
      // The 1.17.11 server accepts `limit`, but its generated v1 SDK query type
      // omits the field. A named structural value keeps this compatibility seam
      // at the adapter boundary and preserves the previous bounded snapshot.
      const query = {
        directory: pluginDirectory,
        limit: SESSION_BOOTSTRAP_LIMIT,
      };
      const response = await client.session.list({
        query,
        signal,
      });
      return responseData<Session[]>(response, "session list").map(reporterSession);
    },
    async statuses(signal) {
      const response = await client.session.status({
        query: { directory: pluginDirectory },
        signal,
      });
      return responseData<Record<string, unknown>>(response, "session status");
    },
    async messages(session, signal) {
      const response = await client.session.messages({
        path: { id: session.id },
        query: {
          directory: clean(session.directory) ?? pluginDirectory,
          limit: 50,
        },
        signal,
      });
      return responseData<Array<{ info: Message }>>(response, "session messages").map(({ info }) =>
        reporterMessage(info),
      );
    },
    async modelLimits(directory, signal) {
      const response = await client.config.providers({
        query: { directory: clean(directory) ?? pluginDirectory },
        signal,
      });
      const providers = responseData<{ providers: Provider[] }>(
        response,
        "provider configuration",
      ).providers;
      const limits = new Map<string, number>();
      for (const provider of providers) {
        for (const [modelID, model] of Object.entries(provider.models)) {
          const limit = model.limit.context;
          if (limit > 0) {
            limits.set(`${provider.id}/${modelID}`, limit);
          }
        }
      }
      return limits;
    },
  };
}

function createLogger(client: OpenCodeClient, directory: string): ReporterLogger {
  return (entry) => {
    try {
      void client.app
        .log({
          body: {
            service: "kmux-status-server",
            level: entry.level,
            message: entry.message,
            extra: entry.extra,
          },
          query: { directory },
        })
        .catch(() => {
          // This is the final non-throwing boundary for diagnostic transport.
        });
    } catch {
      // Keep OpenCode usable even if constructing a diagnostic request fails.
    }
  };
}

function producerInstance(serverUrl: URL, directory: string): string {
  const normalizedUrl = new URL(serverUrl);
  normalizedUrl.username = "";
  normalizedUrl.password = "";
  const server = normalizedUrl.toString().replace(/\/$/, "") || "default";
  return JSON.stringify([server, directory]);
}

const server: Plugin = async (input: PluginInput) => {
  const reporter = new KmuxServerReporter({
    producerInstance: producerInstance(input.serverUrl, input.directory),
    fallbackDirectory: input.directory,
    source: createReporterSource(input.client, input.directory),
    runCommand: runKmux,
    log: createLogger(input.client, input.directory),
  });
  void reporter.start();

  return {
    event: ({ event }) => reporter.event(event),
    dispose: () => reporter.dispose(),
  };
};

export default {
  id: "kmux-status-server",
  server,
};
