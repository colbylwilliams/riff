import type { SessionDefaults, ToolDefinition } from "@riff/core";
import { buildOpenAISession } from "./session-config.ts";

export interface MintClientSecretOptions {
  /** Server side only. This must never reach a phone or a browser. */
  apiKey: string;
  session: SessionDefaults;
  instructions: string;
  tools: ToolDefinition[];
  vocabulary?: string[];
  model?: string;
  /** 10 to 7200 seconds. The API default is 600. */
  expiresInSeconds?: number;
  baseUrl?: string;
  /** Hashed user id for abuse monitoring. Never send a raw identifier. */
  safetyIdentifier?: string;
  signal?: AbortSignal;
}

export interface ClientSecret {
  value: string;
  expiresAt?: number;
}

const DEFAULT_BASE_URL = "https://api.openai.com/v1";

/**
 * Mints an ephemeral client secret so a device can open a realtime connection without ever holding
 * a real API key.
 *
 * Run this behind your own authenticated endpoint. It is the one part of the provider that must
 * stay on a server, and the whole reason Riff's client side never sees a long-lived credential.
 */
export async function mintClientSecret(options: MintClientSecretOptions): Promise<ClientSecret> {
  const session = buildOpenAISession({
    session: options.session,
    instructions: options.instructions,
    tools: options.tools,
    ...(options.vocabulary ? { vocabulary: options.vocabulary } : {}),
    ...(options.model ? { model: options.model } : {}),
  });

  const response = await fetch(`${options.baseUrl ?? DEFAULT_BASE_URL}/realtime/client_secrets`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${options.apiKey}`,
      "Content-Type": "application/json",
      ...(options.safetyIdentifier ? { "OpenAI-Safety-Identifier": options.safetyIdentifier } : {}),
    },
    body: JSON.stringify({
      ...(options.expiresInSeconds
        ? { expires_after: { anchor: "created_at", seconds: options.expiresInSeconds } }
        : {}),
      session,
    }),
    ...(options.signal ? { signal: options.signal } : {}),
  });

  if (!response.ok) {
    let detail = "";
    try {
      detail = `: ${(await response.text()).slice(0, 300)}`;
    } catch {
      detail = "";
    }
    throw new Error(`could not mint a realtime client secret (${response.status})${detail}`);
  }

  const body = (await response.json()) as { value?: string; expires_at?: number };
  if (!body.value) throw new Error("the client secret response had no value");

  return {
    value: body.value,
    ...(typeof body.expires_at === "number" ? { expiresAt: body.expires_at } : {}),
  };
}
