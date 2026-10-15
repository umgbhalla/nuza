import type { AgentConfig, ModelInfo } from "./protocol";

/**
 * The model chosen for each vault, for one agent.
 *
 * A model id means nothing to a different agent - `sonnet` is not a Codex
 * model - so the choices are kept together with the agent they were made
 * for, and are forgotten the moment that agent changes.
 */
export interface ModelChoices {
  agent: string | null;
  byVault: Record<string, string>;
}

export const NO_MODEL_CHOICES: ModelChoices = { agent: null, byVault: {} };

/** Which agent a choice belongs to: its kind, or a custom agent's command. */
export function agentKey(config: AgentConfig | null): string | null {
  if (!config) return null;
  return config.kind === "custom" ? `custom:${config.command.trim()}` : config.kind;
}

/** The model chosen for `vault`, if one was chosen for this agent. */
export function modelFor(
  choices: ModelChoices,
  agent: AgentConfig | null,
  vault: string | null
): string | null {
  const key = agentKey(agent);
  if (!key || choices.agent !== key) return null;
  return choices.byVault[vault ?? ""] ?? null;
}

/** `choices`, with `model` chosen for `vault` - and every other choice dropped if the agent changed. */
export function chooseModel(
  choices: ModelChoices,
  agent: AgentConfig | null,
  vault: string | null,
  model: string | null
): ModelChoices {
  const key = agentKey(agent);
  const byVault = choices.agent === key ? { ...choices.byVault } : {};
  if (model) byVault[vault ?? ""] = model;
  else delete byVault[vault ?? ""];
  return { agent: key, byVault };
}

/**
 * How costly a model looks from what it is called: free first, then the
 * small ones, then the rest; and, where a model is offered at several
 * reasoning efforts (`gpt-5.5[low]`), the least effort first. Agents say
 * nothing about price, so the name is all there is to go on.
 */
function cost(model: ModelInfo): number {
  const name = `${model.id} ${model.name}`.toLowerCase();
  let score = 5;
  // A gateway of its own that needs a subscription, whatever the model says:
  // v1 listed these as `opencode-go/…`. v2 names its own gateway `opencode/`,
  // and everything it serves there is free, so the name decides.
  if (/^opencode-go\//.test(model.id)) score = 4;
  else if (/\bfree\b|-free\b|big-pickle/.test(name)) score = 0;
  else if (/\bnano\b|-nano\b/.test(name)) score = 1;
  else if (/haiku|mini|flash|lite|lightning|small/.test(name)) score = 2;
  if (/\[(minimal|none)\]/.test(name)) score -= 0.3;
  else if (/\[low\]/.test(name)) score -= 0.2;
  else if (/\[(high|xhigh|max)\]/.test(name)) score += 0.5;
  return score;
}

/**
 * The cheapest of the models an agent offers - what nuza answers with until
 * the writer picks another. Never the agent's "default" entry, which is
 * whatever the agent thinks best, not least. Ties go to the one listed first.
 */
export function cheapestModel(models: ModelInfo[]): string | null {
  let best: ModelInfo | null = null;
  for (const model of models) {
    if (model.id === "default") continue;
    if (!best || cost(model) < cost(best)) best = model;
  }
  return best?.id ?? null;
}

/**
 * The model to answer with in `vault`: the one chosen, while the agent still
 * offers it, or else the cheapest it offers.
 */
export function modelToUse(
  choices: ModelChoices,
  agent: AgentConfig | null,
  vault: string | null,
  models: ModelInfo[]
): string | null {
  const chosen = modelFor(choices, agent, vault);
  if (chosen && (!models.length || models.some((model) => model.id === chosen))) return chosen;
  return cheapestModel(models);
}
