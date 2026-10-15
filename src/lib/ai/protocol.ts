/**
 * The agent, as the webview sees it: the shapes of what the Rust commands in
 * `src-tauri/src/agent/commands.rs` take and give back.
 *
 * Events come straight off a Rust enum, so their fields are snake_case
 * (`stop_reason`) where the rest of the app's IPC is camelCase. That is on
 * purpose - mirrored here as it is, rather than renamed on one side and not
 * the other.
 */

/** Which agent is turned on. Mirrors `AgentConfig` in agent/config.rs. */
export type AgentConfig =
  | { kind: "claudeCode" }
  | { kind: "codex" }
  | { kind: "openCode" }
  | { kind: "custom"; command: string; args: string[]; dataDirs: string[] };

export type AgentKind = AgentConfig["kind"];

export type AgentErrorKind =
  | "off" // no agent is turned on
  | "agent_missing" // the program is not there, or would not start
  | "not_signed_in" // the agent has no sign-in
  | "usage_limit" // the person's own plan is used up for now
  | "model_unavailable" // their plan cannot use the model that was picked
  | "adapter_crashed" // the agent stopped in the middle of things
  | "timeout" // it went quiet and was given up on
  | "other";

export interface AgentError {
  kind: AgentErrorKind;
  message: string;
}

export interface PlanEntry {
  content: string;
  status: string;
}

/** Everything an agent says during a turn. Mirrors `AgentEvent` in agent/events.rs. */
export type AgentEvent =
  | { kind: "token"; text: string }
  | { kind: "thought"; text: string }
  | { kind: "step"; id: string; title: string | null; status: string | null }
  | { kind: "plan"; entries: PlanEntry[] }
  | { kind: "usage"; used: number; size: number }
  /** Finishes "nuza didn't let the agent …". */
  | { kind: "refused"; action: string }
  | { kind: "notice"; text: string }
  | { kind: "done"; stop_reason: string }
  | { kind: "failed"; code: AgentErrorKind; message: string };

/** Where the AI settings stand for this window. Mirrors `AgentState`. */
export interface AgentState {
  agent: AgentConfig | null;
  label: string | null;
  sandboxed: boolean;
  /** Why it is not fenced in, where that is worth saying. */
  unfencedBecause: string | null;
  webLookups: boolean;
  /** Whether the agent may run commands and change notes, everywhere. */
  fullAccess: boolean;
  hasVault: boolean;
  /** Whether the agent may search this vault with nuza's read-only tools. */
  vaultSearch: boolean;
  /** Whether the running agent can take those tools; null before it has started. */
  agentSearches: boolean | null;
}

/** What is installed on this computer. Mirrors `Detection` in agent/status.rs. */
export interface Detection {
  /** Node's version, when `npx` is there too. */
  node: string | null;
  agents: Found[];
}

export interface Found {
  id: PresetId;
  path: string | null;
  version: string | null;
  /** `null` is "nuza cannot tell" - never to be shown as signed out. */
  signedIn: boolean | null;
}

export type PresetId = "claude-code" | "codex" | "opencode";

export interface ModelInfo {
  id: string;
  name: string;
  description: string;
}

/** Whether something a command rejected with is the agent's error shape. */
export function isAgentError(value: unknown): value is AgentError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AgentError).kind === "string" &&
    typeof (value as AgentError).message === "string"
  );
}
