import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, LoaderCircle, X } from "lucide-react";
import { cn } from "cn";
import { Switch } from "@/components/ui/switch";
import { useAgent } from "@/hooks/useAgent";
import {
  AI_DOCS_URL,
  PRESETS,
  presetFor,
  readiness,
  sameAgent,
  splitArgs,
  splitDirs,
  startingNote,
  type Preset,
  type Readiness,
} from "@/lib/ai/agents";
import { cheapestModel, chooseModel, modelFor, type ModelChoices } from "@/lib/ai/modelChoice";
import {
  CONVERSATION_CLEARED,
  MEMORY_ITEM_LENGTH,
  forget,
  memoryFor,
  remember,
  saveConversation,
  saveThreads,
  THREAD_DAYS,
  type MemoryStore,
} from "@/lib/ai/memory";
import { INSTRUCTIONS_LIMIT } from "@/lib/ai/prompt";
import type { AgentConfig, ModelInfo } from "@/lib/ai/protocol";
import AgentTryOut from "./AgentTryOut";
import SettingRow from "./SettingRow";

interface AiSettingsProps {
  /** The open vault, for the settings that are per vault. */
  vaultPath: string | null;
  sendWholeNote: boolean;
  setSendWholeNote: (enabled: boolean) => void;
  modelChoices: ModelChoices;
  setModelChoices: (choices: ModelChoices) => void;
  instructions: string;
  setInstructions: (instructions: string) => void;
  memory: MemoryStore;
  setMemory: (memory: MemoryStore) => void;
}

/**
 * How the AI replies: the writer's own instructions, what it should keep in
 * mind in this vault, and the conversation it remembers.
 */
function Replies({
  vaultPath,
  instructions,
  setInstructions,
  memory,
  setMemory,
}: Pick<AiSettingsProps, "vaultPath" | "instructions" | "setInstructions" | "memory" | "setMemory">) {
  const [adding, setAdding] = useState("");
  const kept = memoryFor(memory, vaultPath);
  const field =
    "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-white outline-none focus:border-[var(--nuza-accent)]";
  const add = () => {
    if (!adding.trim()) return;
    setMemory(remember(memory, vaultPath, adding));
    setAdding("");
  };
  return (
    <section className="space-y-4 border-t border-zinc-800 pt-3">
      <div className="space-y-1.5">
        <h3 className="text-sm font-medium text-white">How it replies</h3>
        <p className="text-xs text-gray-500">
          Sent with every question, as your preferences. They can&apos;t change what the agent is allowed to
          do.
        </p>
        <textarea
          value={instructions}
          maxLength={INSTRUCTIONS_LIMIT}
          onChange={(event) => setInstructions(event.target.value)}
          placeholder="Reply briefly, in British English. Keep my tone casual."
          className={cn(field, "h-20 resize-none")}
        />
        <p className="text-right text-[11px] text-gray-600">
          {instructions.length}/{INSTRUCTIONS_LIMIT}
        </p>
      </div>

      <div className="space-y-1.5">
        <h3 className="text-sm font-medium text-white">Memory</h3>
        <p className="text-xs text-gray-500">
          {vaultPath
            ? "Things to keep in mind in this folder. You can also type /remember in the panel."
            : "Things to keep in mind when no folder is open."}
        </p>
        {kept.length > 0 && (
          <ul className="space-y-1">
            {kept.map((item, index) => (
              <li
                key={`${index}:${item}`}
                className="flex items-start justify-between gap-2 rounded-md bg-zinc-800/50 px-2 py-1 text-xs text-gray-300"
              >
                <span className="min-w-0 break-words">{item}</span>
                <button
                  type="button"
                  onClick={() => setMemory(forget(memory, vaultPath, index))}
                  title="Forget this"
                  aria-label={`Forget “${item}”`}
                  className="shrink-0 cursor-pointer text-gray-500 hover:text-white"
                >
                  <X size={12} />
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="flex gap-2">
          <input
            value={adding}
            maxLength={MEMORY_ITEM_LENGTH}
            onChange={(event) => setAdding(event.target.value)}
            onKeyDown={(event) => event.key === "Enter" && add()}
            placeholder="The project is called Tangerine"
            className={field}
          />
          <button type="button" className={cn(BUTTON, "shrink-0")} disabled={!adding.trim()} onClick={add}>
            Remember
          </button>
        </div>
      </div>

      <SettingRow
        title="Chats"
        description={`Kept on this computer, so you can go back to one from the panel. Chats untouched for ${THREAD_DAYS} days are deleted.`}
      >
        <button
          type="button"
          className={BUTTON}
          onClick={() => {
            saveConversation(vaultPath, []);
            saveThreads(vaultPath, []);
            window.dispatchEvent(new CustomEvent(CONVERSATION_CLEARED, { detail: vaultPath }));
          }}
        >
          Delete all
        </button>
      </SettingRow>
    </section>
  );
}

const BUTTON =
  "cursor-pointer rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1 text-xs text-white transition-colors hover:border-[var(--nuza-accent)] disabled:cursor-default disabled:opacity-60";

/** A home folder written the way it would be typed. */
function tidy(path: string) {
  return path.replace(/^(?:\/(?:Users|home)\/|[A-Za-z]:\\Users\\)[^/\\]+/, "~");
}

function Code({ children }: { children: string }) {
  return (
    <code className="rounded bg-zinc-800/80 px-1 py-0.5 font-mono text-[11px] text-gray-300">{children}</code>
  );
}

/** A line of copy with `backticks` drawn as code. */
function Instructions({ text }: { text: string }) {
  return (
    <>
      {text
        .split(/(`[^`]+`)/)
        .map((piece, index) =>
          piece.startsWith("`") ? <Code key={index}>{piece.slice(1, -1)}</Code> : piece
        )}
    </>
  );
}

function Status({ found, onLogin }: { found: Readiness; onLogin?: () => void }) {
  switch (found.state) {
    case "unknown":
      return <span className="text-gray-500">Looking…</span>;
    case "missing":
      return <span className="text-gray-500">Not found</span>;
    case "needs-node":
      return <span className="text-amber-400">Found, but Node.js is missing</span>;
    case "found":
      return (
        <span className="text-gray-400">
          Found · <span className="font-mono">{tidy(found.path)}</span>
          {/* Only a "no" is said: a "can't tell" is not a signed-out agent. */}
          {found.signedIn === false && (
            <>
              {" · "}
              <span className="text-amber-400">not signed in</span>
              {onLogin && (
                <button
                  type="button"
                  onClick={onLogin}
                  className="ml-1.5 cursor-pointer text-[var(--nuza-accent)] hover:underline"
                >
                  Sign in
                </button>
              )}
            </>
          )}
        </span>
      );
  }
}

interface AgentRowProps {
  preset: Preset;
  found: Readiness;
  isOn: boolean;
  isStarting: boolean;
  busy: boolean;
  onUse: () => void;
  onLogin: () => void;
}

function AgentRow({ preset, found, isOn, isStarting, busy, onUse, onLogin }: AgentRowProps) {
  const usable = found.state === "found";
  return (
    <div className="flex items-start justify-between gap-4 py-3">
      <div className="min-w-0 space-y-1">
        <h3 className="flex items-center gap-2 text-sm font-medium text-white">
          {preset.label}
          {isOn && <Check size={13} className="text-[var(--nuza-accent)]" aria-label="In use" />}
        </h3>
        <p className="text-xs">
          <Status found={found} onLogin={preset.id === "codex" ? onLogin : undefined} />
        </p>
        <p className="text-xs text-gray-500">
          {preset.blurb} {preset.needs}
        </p>
        {found.state === "missing" && (
          <p className="text-xs text-gray-500">
            Install it with <Code>{preset.install}</Code>, then{" "}
            <Instructions text={lowerFirst(preset.signIn)} />
          </p>
        )}
        {found.state === "needs-node" && (
          <p className="text-xs text-gray-500">Install Node.js 22 or newer, or open nuza from a terminal.</p>
        )}
      </div>
      <div className="shrink-0 pt-0.5">
        {isOn ? (
          <span className="text-xs text-gray-400">In use</span>
        ) : (
          <button type="button" className={BUTTON} disabled={busy || !usable} onClick={onUse}>
            {isStarting ? "Starting…" : "Use"}
          </button>
        )}
      </div>
    </div>
  );
}

function lowerFirst(text: string) {
  return text.charAt(0).toLowerCase() + text.slice(1);
}

interface CustomAgentProps {
  current: AgentConfig | null;
  busy: boolean;
  onUse: (config: AgentConfig) => void;
}

/** Any program that speaks ACP: its command, its arguments, and where it keeps its own things. */
function CustomAgent({ current, busy, onUse }: CustomAgentProps) {
  const kept = current?.kind === "custom" ? current : null;
  const [command, setCommand] = useState(kept?.command ?? "");
  const [args, setArgs] = useState(kept?.args.join(" ") ?? "");
  const [dirs, setDirs] = useState(kept?.dataDirs.join("\n") ?? "");
  const [where, setWhere] = useState<string | null | undefined>(undefined);

  // Whether the command is there, as it is typed - a moment after the typing
  // stops, so every keystroke is not a search of PATH.
  useEffect(() => {
    if (!command.trim()) {
      setWhere(undefined);
      return;
    }
    let current = true;
    const timer = window.setTimeout(() => {
      invoke<string | null>("agent_find_command", { command })
        .then((found) => current && setWhere(found))
        .catch(() => current && setWhere(null));
    }, 250);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [command]);

  const config: AgentConfig = {
    kind: "custom",
    command: command.trim(),
    args: splitArgs(args),
    dataDirs: splitDirs(dirs),
  };
  const isOn = sameAgent(current, config);
  const field =
    "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono text-xs text-white outline-none focus:border-[var(--nuza-accent)]";

  return (
    <div className="space-y-2 py-3">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h3 className="flex items-center gap-2 text-sm font-medium text-white">
            Something else
            {isOn && <Check size={13} className="text-[var(--nuza-accent)]" aria-label="In use" />}
          </h3>
          <p className="mt-1 text-xs text-gray-500">Any program that speaks ACP over its input and output.</p>
        </div>
        <button
          type="button"
          className={cn(BUTTON, "shrink-0")}
          disabled={busy || !command.trim() || !where || isOn}
          onClick={() => onUse(config)}
        >
          {isOn ? "In use" : "Use"}
        </button>
      </div>
      <label className="block space-y-1">
        <span className="flex justify-between text-xs text-gray-400">
          Command
          {where !== undefined && (
            <span className={where ? "text-gray-500" : "text-amber-400"}>
              {where ? `found · ${tidy(where)}` : "not found"}
            </span>
          )}
        </span>
        <input
          className={field}
          value={command}
          onChange={(event) => setCommand(event.target.value)}
          placeholder="node"
          spellCheck={false}
        />
      </label>
      <label className="block space-y-1">
        <span className="text-xs text-gray-400">Arguments</span>
        <input
          className={field}
          value={args}
          onChange={(event) => setArgs(event.target.value)}
          placeholder="~/agents/my-agent.js --acp"
          spellCheck={false}
        />
      </label>
      <label className="block space-y-1">
        <span className="text-xs text-gray-400">Its data folders, one per line</span>
        <textarea
          className={cn(field, "h-14 resize-none")}
          value={dirs}
          onChange={(event) => setDirs(event.target.value)}
          placeholder="~/.config/my-agent"
          spellCheck={false}
        />
      </label>
    </div>
  );
}

/**
 * The AI pane. AI is off until the writer turns it on here: before that this
 * pane is the switch and a line on what it does, and nothing else - nothing
 * looks for an agent, and nothing is started.
 */
export default function AiSettings({
  enabled,
  setEnabled,
  ...settings
}: AiSettingsProps & { enabled: boolean; setEnabled: (enabled: boolean) => void }) {
  return (
    <div className="space-y-6">
      <SettingRow
        title="AI"
        description="An optional panel (⌘L) that asks an AI agent already on your computer about your notes, on your own plan. Off, nuza never looks for or starts one."
      >
        <Switch checked={enabled} onCheckedChange={setEnabled} aria-label="AI" />
      </SettingRow>
      {enabled && <AgentSettings {...settings} />}
    </div>
  );
}

/**
 * Which agent nuza talks to, what it is sent, and a place to try it. Shown
 * only with AI on: showing it is what lets nuza look for agents at all.
 */
function AgentSettings({
  vaultPath,
  sendWholeNote,
  setSendWholeNote,
  modelChoices,
  setModelChoices,
  instructions,
  setInstructions,
  memory,
  setMemory,
}: AiSettingsProps) {
  const {
    state,
    detection,
    detecting,
    starting,
    error,
    detect,
    use,
    turnOff,
    setWebLookups,
    setVaultSearch,
    setFullAccess,
    login,
    listModels,
  } = useAgent();
  const [models, setModels] = useState<ModelInfo[] | null>(null);

  useEffect(() => {
    void detect();
  }, [detect]);

  const agent = state?.agent ?? null;
  const preset = presetFor(agent);
  const busy = starting !== null;

  // The models the agent offers, once it is on. Listing them starts it.
  useEffect(() => {
    setModels(null);
    if (!agent) return;
    let current = true;
    void listModels().then((offered) => current && setModels(offered));
    return () => {
      current = false;
    };
  }, [agent, listModels]);

  const chosenModel = modelFor(modelChoices, agent, vaultPath);
  const cheapest = cheapestModel(models ?? []);
  const sandboxLine = useMemo(() => {
    if (!state) return null;
    return state.sandboxed
      ? "Agents run fenced in on this computer: they see only what nuza sends them, and can't change your notes or any of your files."
      : `This computer can't fence the agent in, so it runs with your own permissions.${
          state.unfencedBecause ? ` ${state.unfencedBecause}` : ""
        }`;
  }, [state]);

  return (
    <div className="space-y-6">
      <section>
        <p className="text-xs text-gray-500">
          nuza doesn&apos;t run its own AI. It uses an agent already on your computer, on your own plan —
          nothing to sign up for.
        </p>
        <div className="divide-y divide-zinc-800">
          {PRESETS.map((preset) => (
            <AgentRow
              key={preset.id}
              preset={preset}
              found={readiness(preset, detection)}
              isOn={sameAgent(agent, preset.config)}
              isStarting={sameAgent(starting, preset.config)}
              busy={busy}
              onUse={() => void use(preset.config)}
              onLogin={() => void login()}
            />
          ))}
          <CustomAgent current={agent} busy={busy} onUse={(config) => void use(config)} />
        </div>
        {starting && <p className="mt-1 text-xs text-gray-500">{startingNote(starting)}</p>}
        {error && (
          <p role="alert" className="mt-1 text-xs text-amber-400">
            {error.message}
          </p>
        )}
        <div className="mt-2 flex items-center justify-between gap-4">
          <p className="text-xs text-gray-500">{sandboxLine}</p>
          <div className="flex shrink-0 gap-2">
            <button type="button" className={BUTTON} disabled={detecting} onClick={() => void detect()}>
              {detecting ? (
                <LoaderCircle size={12} className="animate-spin" aria-label="Looking" />
              ) : (
                "Look again"
              )}
            </button>
            {agent && (
              <button type="button" className={BUTTON} onClick={() => void turnOff()}>
                Turn off
              </button>
            )}
          </div>
        </div>
      </section>

      <section className="divide-y divide-zinc-800 border-t border-zinc-800">
        <SettingRow
          title="Send the whole note"
          description="Off sends only the text you select — for help with one sentence of a private note."
        >
          <Switch checked={sendWholeNote} onCheckedChange={setSendWholeNote} />
        </SettingRow>
        <SettingRow
          title="Let the agent search this vault"
          description={
            state?.hasVault
              ? state.agentSearches === false
                ? "This agent can't use nuza's search tools, so it only sees the note text that is sent."
                : "It can find and read other notes in this folder with nuza's read-only tools, and cite them."
              : "Open a folder to choose this for it."
          }
        >
          <Switch
            checked={state?.vaultSearch ?? false}
            disabled={!state?.hasVault}
            onCheckedChange={(allowed) => void setVaultSearch(allowed)}
          />
        </SettingRow>
        <SettingRow
          title="Look things up on the web"
          description={
            state?.hasVault
              ? "For this folder only. A note could talk the agent into sending what it read to a website, so this stays off unless you need it."
              : "Open a folder to choose this for it."
          }
        >
          <Switch
            checked={state?.webLookups ?? false}
            disabled={!state?.hasVault || !agent}
            onCheckedChange={(allowed) => void setWebLookups(allowed)}
          />
        </SettingRow>
        <SettingRow
          title="Full access"
          description="Lets the agent run commands on this computer and change, move and delete notes it can reach. Deleting moves a note to the trash; nothing else here can be undone. Off unless you need it."
        >
          <Switch
            checked={state?.fullAccess ?? false}
            disabled={!agent}
            onCheckedChange={(allowed) => void setFullAccess(allowed)}
          />
        </SettingRow>
        {agent && models && models.length > 0 && (
          <SettingRow
            title="Model"
            description={`The model ${state?.label ?? "the agent"} answers with in this folder. Unless you pick one, nuza uses the cheapest it offers.`}
          >
            <select
              className="max-w-44 cursor-pointer rounded-md border border-zinc-700 bg-zinc-800 px-2 py-1 text-xs text-white outline-none focus:border-[var(--nuza-accent)]"
              value={chosenModel ?? ""}
              onChange={(event) =>
                setModelChoices(chooseModel(modelChoices, agent, vaultPath, event.target.value || null))
              }
            >
              <option value="">
                Cheapest
                {cheapest ? ` (${models.find((model) => model.id === cheapest)?.name ?? cheapest})` : ""}
              </option>
              {models.map((model) => (
                <option key={model.id} value={model.id} title={model.description}>
                  {model.name}
                </option>
              ))}
            </select>
          </SettingRow>
        )}
      </section>

      <Replies
        vaultPath={vaultPath}
        instructions={instructions}
        setInstructions={setInstructions}
        memory={memory}
        setMemory={setMemory}
      />

      <section className="border-t border-zinc-800 pt-3">
        <h3 className="text-sm font-medium text-white">What gets sent</h3>
        <p className="mt-1 text-xs text-gray-500">
          Your questions, the text you select, the note you&apos;re asking about, your instructions and memory
          go to the agent program on this computer{preset ? `, ${preset.label},` : ""} which sends them on to
          its own provider, on your own account. nuza has no server: the conversation and memory stay on this
          computer.{" "}
          <button
            type="button"
            className="cursor-pointer text-[var(--nuza-accent)] hover:underline"
            onClick={() => void openUrl(AI_DOCS_URL)}
          >
            Learn more
          </button>
        </p>
      </section>

      {agent && state?.label && (
        <section className="border-t border-zinc-800 pt-3">
          <AgentTryOut label={state.label} model={chosenModel} />
        </section>
      )}
    </div>
  );
}
