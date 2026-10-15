import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { reasonFor, report } from "@/lib/notices";
import {
  isAgentError,
  type AgentConfig,
  type AgentError,
  type AgentState,
  type Detection,
  type ModelInfo,
} from "@/lib/ai/protocol";

/** Whatever a command rejected with, as an agent error. */
export function asAgentError(error: unknown): AgentError {
  return isAgentError(error) ? error : { kind: "other", message: reasonFor(error) };
}

/**
 * The agent, as far as Settings and the panel are concerned: which one is
 * on, what this computer has, and turning one on or off.
 *
 * Nothing is looked for until `detect` is called, which is only done where
 * the person has gone looking for AI - the settings pane, the panel. A nuza
 * that is never asked about AI never runs a single `--version`.
 */
export function useAgent() {
  const [state, setState] = useState<AgentState | null>(null);
  const [detection, setDetection] = useState<Detection | null>(null);
  const [detecting, setDetecting] = useState(false);
  /** The agent being turned on, while it starts. */
  const [starting, setStarting] = useState<AgentConfig | null>(null);
  /** Why the last attempt to turn one on, or to start it, went wrong. */
  const [error, setError] = useState<AgentError | null>(null);

  const refresh = useCallback(async () => {
    try {
      setState(await invoke<AgentState>("agent_state"));
    } catch (error) {
      console.error("Couldn't read the AI settings:", error);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const detect = useCallback(async () => {
    setDetecting(true);
    try {
      setDetection(await invoke<Detection>("agent_detect"));
    } catch (error) {
      console.error("Couldn't look for agents:", error);
    } finally {
      setDetecting(false);
    }
  }, []);

  /** Turns `config` on - after the one-time notice, if it has not been agreed to - and starts it. */
  const use = useCallback(
    async (config: AgentConfig) => {
      setStarting(config);
      setError(null);
      try {
        await invoke<boolean>("agent_use", { config });
      } catch (error) {
        // It is turned on even so; what failed is starting it, and the
        // person is shown why where they can fix it.
        setError(asAgentError(error));
      } finally {
        setStarting(null);
        await refresh();
      }
    },
    [refresh]
  );

  const turnOff = useCallback(async () => {
    setError(null);
    try {
      await invoke("agent_turn_off");
    } catch (error) {
      report("Couldn't turn the agent off", error);
    }
    await refresh();
  }, [refresh]);

  /** Starts the agent ahead of the first question. */
  const warm = useCallback(async () => {
    try {
      await invoke("agent_warm");
      setError(null);
    } catch (error) {
      const failed = asAgentError(error);
      if (failed.kind !== "off") setError(failed);
    }
  }, []);

  /** Lets the agent search this window's vault with nuza's read-only tools, or stops it. */
  const setVaultSearch = useCallback(
    async (allowed: boolean) => {
      try {
        await invoke("agent_set_vault_search", { allowed });
      } catch (error) {
        report("Couldn't change vault search", error);
      }
      await refresh();
    },
    [refresh]
  );

  const setWebLookups = useCallback(
    async (allowed: boolean) => {
      try {
        await invoke<boolean>("agent_set_web_lookups", { allowed });
      } catch (error) {
        report("Couldn't change web lookups", error);
      }
      await refresh();
    },
    [refresh]
  );

  /** Lets the agent run commands and change notes, or stops it. */
  const setFullAccess = useCallback(
    async (allowed: boolean) => {
      try {
        await invoke<boolean>("agent_set_full_access", { allowed });
      } catch (error) {
        report("Couldn't change full access", error);
      }
      await refresh();
    },
    [refresh]
  );

  /** Starts `codex login`, and looks again once it has had time to open a browser. */
  const login = useCallback(async () => {
    try {
      await invoke("agent_login");
      window.setTimeout(() => void detect(), 5000);
    } catch (error) {
      report("Couldn't start the sign-in", error);
    }
  }, [detect]);

  const listModels = useCallback(async (): Promise<ModelInfo[]> => {
    try {
      return await invoke<ModelInfo[]>("agent_list_models");
    } catch (error) {
      const failed = asAgentError(error);
      if (failed.kind !== "off") setError(failed);
      return [];
    }
  }, []);

  return {
    state,
    detection,
    detecting,
    starting,
    error,
    refresh,
    detect,
    use,
    turnOff,
    warm,
    setWebLookups,
    setVaultSearch,
    setFullAccess,
    login,
    listModels,
  };
}
