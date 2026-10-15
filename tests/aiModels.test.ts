import { describe, expect, test } from "bun:test";
import { NO_MODEL_CHOICES, cheapestModel, chooseModel, modelToUse } from "../src/lib/ai/modelChoice";
import type { ModelInfo } from "../src/lib/ai/protocol";

const offered = (...ids: [string, string][]): ModelInfo[] =>
  ids.map(([id, name]) => ({ id, name, description: "" }));

// As the real agents list them.
const claude = offered(
  ["default", "Default (recommended)"],
  ["opus", "Opus 5.5"],
  ["sonnet", "Sonnet 5.5"],
  ["fable", "Fable 5.1"],
  ["haiku", "Haiku 5.5"],
  ["claude-haiku-4-5-20251001", "Haiku 4.5"]
);
const opencode = offered(
  // As OpenCode v2 lists them: everything on its own gateway is free.
  ["opencode/cat-jam-free", "Cat Jam Free"],
  ["opencode/deepseek-v4.1-flash-free", "DeepSeek V4.1 Flash Free"],
  ["opencode/mimo-v2.6-flash-free", "MiMo-V2.6-Flash Free"],
  ["opencode/nemotron-3.5-lightning-free", "Nemotron 3.5 Lightning Free"],
  ["opencode/big-pickle", "Big Pickle"]
);
const codex = offered(
  ["gpt-5.5[high]", "gpt-5.5 (high)"],
  ["gpt-5.5[low]", "gpt-5.5 (low)"],
  ["gpt-5.5-mini[medium]", "gpt-5.5-mini (medium)"],
  ["gpt-5.5-mini[low]", "gpt-5.5-mini (low)"]
);

describe("the cheapest model", () => {
  test("for Claude Code is the newest Haiku, never the default entry", () => {
    expect(cheapestModel(claude)).toBe("haiku");
  });

  test("for OpenCode is a free one, not the agent's default entry", () => {
    expect(cheapestModel(opencode)).toBe("opencode/cat-jam-free");
  });

  test("is a free one, not a gateway that needs a subscription", () => {
    // v1 listed its paying gateway beside the free models, first.
    const withPaidGateway = offered(
      ["opencode-go/cat-jam-free", "OpenCode Go/Cat Jam Free"],
      ["opencode/big-pickle", "OpenCode/Big Pickle"]
    );
    expect(cheapestModel(withPaidGateway)).toBe("opencode/big-pickle");
  });

  test("for Codex is the small model at the least effort", () => {
    expect(cheapestModel(codex)).toBe("gpt-5.5-mini[low]");
  });

  test("with nothing offered there is none, and the agent picks", () => {
    expect(cheapestModel([])).toBeNull();
  });
});

describe("the model a question is answered with", () => {
  const agent = { kind: "claudeCode" } as const;

  test("is the cheapest until the writer picks one", () => {
    expect(modelToUse(NO_MODEL_CHOICES, agent, "/notes", claude)).toBe("haiku");
    const picked = chooseModel(NO_MODEL_CHOICES, agent, "/notes", "opus");
    expect(modelToUse(picked, agent, "/notes", claude)).toBe("opus");
  });

  test("goes back to the cheapest when the one picked is no longer offered", () => {
    const picked = chooseModel(NO_MODEL_CHOICES, agent, "/notes", "claude-opus-4-1");
    expect(modelToUse(picked, agent, "/notes", claude)).toBe("haiku");
  });
});
