# ACP on the wire

What real agents actually send, recorded before anything was designed on top of
it. This is the reference `scripts/mock-acp-agent.mjs` is kept honest against:
when an adapter pin moves, record a new transcript, compare it with this one,
and change the mock (and this page) to match.

Recorded 2026-10-14 on macOS, with nuza's own client sending (the `-> ` lines)
and everything the agent printed on stdout read back (`<- `). Long lines are
cut with `…`; an account's name and email are replaced with `<…>`; the session's
temporary directory is `<tmp>/session`.

| Agent                            | Started as                                                                        | `initialize` answered after                               |
| -------------------------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------- |
| Claude Code (a paid Claude plan) | `npx -y --cache ~/.npm/_nuza-claude @agentclientprotocol/claude-agent-acp@0.90.0` | **103 s** cold (npx fetching the adapter), **2.1 s** warm |
| OpenCode 1.15.13                 | `opencode acp`                                                                    | 20 s (its plugins loading)                                |
| OpenCode 2.0.29                  | `opencode acp`                                                                    | ~0.1 s (see [OpenCode v2](#opencode-v2-2029))             |
| Codex                            | `npx -y @agentclientprotocol/codex-acp@<pin>`                                     | not recorded: no Codex on the recording machine           |

To record again, run the opt-in test, which goes through nuza's own client:

```sh
cargo test --manifest-path src-tauri/Cargo.toml real_claude -- --ignored --nocapture
```

## What it showed

Each of these is a decision made somewhere else in the code, and this is why.

| Seen                                                                                                                                                                                                                                     | What nuza does about it                                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `protocolVersion` is a number in both directions                                                                                                                                                                                         | `initialize` sends `1`, not `"1"`; the mock refuses a string                                                                                                                                                                                               |
| `session/new` takes `mcpServers` even when it is `[]`                                                                                                                                                                                    | Always sent; the mock refuses a session without it                                                                                                                                                                                                         |
| There is no "turn finished" notification. The turn ends when `session/prompt` is answered, with a `stopReason`                                                                                                                           | Every chunk has been read from stdout before the answer to `session/prompt` is, so a "done" the client emits after it can never overtake the last word - provided it goes through the same channel as the words                                            |
| Claude Code has **no** `read-only` mode. `session/set_mode` to it fails with `Internal error` and the reason, `Invalid Mode`, under `data.details`                                                                                       | Errors keep their details (`Internal error: Invalid Mode`). A session is put in `read-only` where the agent has one, otherwise left in (or moved to) a mode that asks before every change, so every change reaches nuza as a permission request it refuses |
| Claude Code's `plan` mode is **not** a read-only mode: in it, Claude wrote a plan file into `~/.claude/plans/` without asking, then asked to leave plan mode                                                                             | `plan` is never chosen. Only the OS sandbox keeps an agent's writes inside its own state folders                                                                                                                                                           |
| OpenCode has no modes at all: `-32602 mode not found`                                                                                                                                                                                    | Asking for a mode is allowed to fail for an agent with none                                                                                                                                                                                                |
| OpenCode (through a plugin) prints a line on stdout that is not JSON                                                                                                                                                                     | Lines that are not JSON-RPC are skipped, not fatal                                                                                                                                                                                                         |
| A permission request from the agent has its own id, starting at `0`                                                                                                                                                                      | The id is echoed back exactly as it came                                                                                                                                                                                                                   |
| Agents send notifications nuza has no use for: `_auth/status_update` (carrying the account's email), `available_commands_update`, `config_option_update`, `session_info_update`, and `usage_update` with a `cost` and rate-limit `_meta` | Dropped by the harness; nothing about the account is kept or shown                                                                                                                                                                                         |
| `usage_update` carries `used` and `size` for the context window                                                                                                                                                                          | The only usage number the panel shows                                                                                                                                                                                                                      |
| Claude Code reports its models as a `configOptions` entry with `category: "model"`; OpenCode the same                                                                                                                                    | Both shapes are read; Codex reports `models.availableModels`                                                                                                                                                                                               |
| Both agents say `mcpCapabilities.http: true`                                                                                                                                                                                             | nuza's vault tools can be offered over loopback HTTP                                                                                                                                                                                                       |
| A cold `npx` start took 103 seconds                                                                                                                                                                                                      | The agent is started in the background as soon as it is turned on, not at the first question                                                                                                                                                               |

## Claude Code

### Starting

```
-> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
<- {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"claudeCode":{"promptQueueing":true},"authStatus":{}},"promptCapabilities":{"image":true,"embeddedContext":true},"mcpCapabilities":{"http":true,"sse":true},"auth":{"logout":{}},"providers":{},"loadSession":true,"sessionCapabilities":{"additionalDirectories":{},"close":{},"delete":{},"fork":{},"list":{},"resume":{}}},"agentInfo":{"name":"@agentclientprotocol/claude-agent-acp","title":"Claude Agent","version":"0.90.0"},"authMethods":[],"_meta":{"steering":{"supported":true}}}}
```

### A session

```
-> {"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"<tmp>/session","mcpServers":[]}}
<- {"jsonrpc":"2.0","method":"_auth/status_update","params":{"authStatus":{"kind":"account","label":"<plan>","account":{"plan":"<plan>","email":"<email>","organization":"<organization>"}}}}
<- {"jsonrpc":"2.0","id":2,"result":{"sessionId":"f525628e-…","modes":{"currentModeId":"default","availableModes":[{"id":"default","name":"Manual","description":"Always ask before making changes"},{"id":"acceptEdits","name":"Accept edits","description":"Automatically accept all file edits"},{"id":"plan","name":"Plan","description":"Create a plan before making changes"},{"id":"auto","name":"Auto","description":"Claude handles permission decisions"},{"id":"bypassPermissions","name":"Bypass permissions","description":"Accepts all permissions"}]},"configOptions":[{"id":"mode",…},{"id":"model","name":"Model","description":"AI model to use","category":"model","type":"select","currentValue":"opus","options":[{"value":"default","name":"Default (recommended)","description":"Opus 5.5"},{"value":"opus","name":"Opus 5.5","description":"For complex work and everyday tasks"},{"value":"sonnet","name":"Sonnet 5.5","description":"Most efficient for simpler tasks"},…]}]}}
-> {"jsonrpc":"2.0","id":3,"method":"session/set_mode","params":{"sessionId":"f525628e-…","modeId":"read-only"}}
<- {"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"Internal error","data":{"details":"Invalid Mode"}}}
```

### A turn

```
-> {"jsonrpc":"2.0","id":5,"method":"session/prompt","params":{"sessionId":"f525628e-…","prompt":[{"type":"text","text":"Say hello in five words or fewer. Do not use any tools."}]}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"available_commands_update","availableCommands":[…]}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"usage_update","used":23400,"size":1000000,"_meta":{"_claude/model":"claude-opus-5-5"}}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello!"},"messageId":"msg_…"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" How"},"messageId":"msg_…"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" can I help?"},"messageId":"msg_…"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"usage_update","used":23464,"size":1000000,"cost":{"amount":0.1214722,"currency":"USD"},"_meta":{…}}}}
<- {"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn","usage":{"inputTokens":2,"outputTokens":66,"cachedReadTokens":8609,"cachedWriteTokens":14787,"totalTokens":23464},"_meta":{…}}}
```

### Asked to write a file, in `plan` mode

The session had been moved to `plan` to see whether it would do as a read-only
mode. It does not: the plan file is written without a permission request.

```
-> {"jsonrpc":"2.0","id":6,"method":"session/prompt","params":{"sessionId":"f525628e-…","prompt":[{"type":"text","text":"Create a file named hello.txt containing the word hi in the current directory, using your file-writing tool."}]}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"session_info_update","title":"Greeting in five words","updatedAt":"…"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"_meta":{"claudeCode":{"toolName":"Write"}},"toolCallId":"toolu_01Eq…","sessionUpdate":"tool_call","name":"Write","rawInput":{},"status":"pending","title":"Preparing file…","kind":"edit","content":[],"locations":[]}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"_meta":{"claudeCode":{"toolName":"Write"}},"toolCallId":"toolu_01Eq…","sessionUpdate":"tool_call_update","rawInput":{"file_path":"~/.claude/plans/say-hello-in-five-immutable-squirrel.md"},"title":"Write ~/.claude/plans/say-hello-in-five-immutable-squirrel.md","locations":[…]}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"_meta":{"claudeCode":{"toolName":"Write"}},"toolCallId":"toolu_01Eq…","sessionUpdate":"tool_call_update","status":"completed","rawOutput":"File created successfully at: ~/.claude/plans/say-hello-in-five-immutable-squirrel.md …"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"_meta":{"claudeCode":{"toolName":"ExitPlanMode"}},"toolCallId":"toolu_01WQ…","sessionUpdate":"tool_call","name":"ExitPlanMode","rawInput":{},"status":"pending","title":"Approve Plan","kind":"switch_mode","content":[]}}}
<- {"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"toolCall":{"toolCallId":"toolu_01WQ…","name":"ExitPlanMode","status":"pending","rawInput":{"plan":"…","planFilePath":"~/.claude/plans/…"},"title":"Approve Plan","kind":"switch_mode","content":[…]},"options":[{"optionId":"exit-plan-default","name":"Yes, manually approve edits","kind":"allow_once"},{"optionId":"exit-plan-clear-auto","name":"Yes, clear context (3% used) and use auto mode","kind":"allow_always"},{"optionId":"exit-plan-auto","name":"Yes, and use auto mode","kind":"allow_always"},…],"sessionId":"f525628e-…"}}
-> {"jsonrpc":"2.0","id":0,"result":{"outcome":{"outcome":"cancelled"}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"_meta":{"claudeCode":{"toolName":"ExitPlanMode","nonExecutionKind":"permission-rule"}},"toolCallId":"toolu_01WQ…","sessionUpdate":"tool_call_update","status":"failed","rawOutput":"Tool permission request failed: Error: Tool use aborted",…}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f525628e-…","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I haven't"},"messageId":"msg_…"}}}
<- … "I haven't created `hello.txt` yet. Plan mode is on, so I can't write files until you approve the plan, and the approval request was cancelled before you could answer." …
<- {"jsonrpc":"2.0","id":6,"result":{"stopReason":"end_turn",…}}
```

The session directory was still empty afterwards.

## OpenCode

```
-> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
   [claude-mem] OpenCode plugin loading (project: opencode)          ← on stdout, not JSON
<- {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"mcpCapabilities":{"http":true,"sse":true},"promptCapabilities":{"embeddedContext":true,"image":true},"sessionCapabilities":{"close":{},"fork":{},"list":{},"resume":{}}},"authMethods":[{"description":"Run `opencode auth login` in the terminal","name":"Login with opencode","id":"opencode-login"}],"agentInfo":{"name":"OpenCode","version":"1.15.13"}}}
-> {"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"<tmp>/session","mcpServers":[]}}
<- {"jsonrpc":"2.0","id":2,"result":{"sessionId":"ses_ec5e…","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"opencode/big-pickle","options":[{"value":"opencode-go/cat-jam-free","name":"OpenCode Go/Cat Jam Free"},…]}]}}
-> {"jsonrpc":"2.0","id":3,"method":"session/set_mode","params":{"sessionId":"ses_ec5e…","modeId":"read-only"}}
<- {"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Invalid params: mode not found: read-only","data":{"mode":"read-only"}}}
-> {"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"ses_ec5e…","prompt":[{"type":"text","text":"Say hello in five words or fewer. Do not use any tools."}]}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_ec5e…","update":{"sessionUpdate":"available_commands_update","availableCommands":[…]}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_ec5e…","update":{"sessionUpdate":"usage_update","used":0,"size":200000,"cost":{"amount":0,"currency":"USD"}}}}
<- {"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn","usage":{"inputTokens":0,"outputTokens":0,"totalTokens":0},"_meta":{}}}
```

The model this OpenCode was configured with answered nothing at all - a turn
that ends `end_turn` with no words. That is a real outcome the panel has to
render, not an error the client can detect.

### OpenCode v2 (2.0.29)

Recorded 2026-10-15, same client. v2 answers `initialize` at once, and its
capabilities have moved on from 1.15.13:

- `mcpCapabilities.sse` is `false` (`http` is still `true`), so nuza registers
  its vault server over the HTTP transport.
- `sessionCapabilities` has grown `additionalDirectories`, `delete` and
  `resume`, alongside the earlier `close`, `fork` and `list`.
- `agentCapabilities._meta` carries
  `{"opencode/child-session-updates":true}`, and `session/prompt` answers with
  `"_meta":{}`.
- There is no `authMethods` and no `agentInfo` any more, and no plugin line on
  stdout: `initialize` is answered in about a tenth of a second, not 20 s.
- Model ids in `configOptions` are `provider/model` - `opencode/big-pickle`,
  `opencode/deepseek-v4.1-flash-free`, `anthropic/claude-fable-5`. v1 listed
  its paying gateway as `opencode-go/…`; v2 names its own gateway `opencode/`,
  and everything it serves there is free.

Two reads at start-up decide the fence, and both are easy to miss, because
neither is mentioned in what the agent says over the protocol:

| Read                                                                  | What the fence does                                                                                                                                                                             |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `~/.claude` and `~/.agents`, for skills and settings - read, not only listed | Both are opened read-only for OpenCode (`read_dirs`). Refuse them and `session/new` fails with `-32603 Internal error: Internal service failure` (`ClientError`, `EPERM: lstat '~/.claude'`) |
| `realPath` of the home folder itself, for its instruction sources      | OpenCode is given the *listing* of home - `(allow file-read-data (literal "…"))`, names only, never a byte inside it. Refuse it and instruction initialization is blocked, the session never drains, and `session/prompt` hangs with no `session/update` at all |

A turn that reaches its model ends normally, with `stopReason: "end_turn"`. A
turn on a model the account cannot use ends just as promptly, with an error on
`session/prompt`:

```
-> {"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"ses_ec0e…","prompt":[{"type":"text","text":"Reply with exactly: PROBE OK"}]}}
<- {"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Authentication required: provider authentication required","data":{}}}
```

That is v2's own default model being one the plan does not cover, not a broken
client: `opencode run` on the same default fails outside nuza too, with `Error:
User not found.`. The opt-in tests take `NUZA_LIVE_MODEL` for exactly this
reason - an agent's configured default is often not a model the account has.

## Codex

Not recorded here: Codex was not installed on the recording machine. The mock's
`codex` flavour follows the Codex adapter's behaviour as it is known from its
own documentation and earlier use - a `read-only` mode, models under
`models.availableModels` with the reasoning effort in brackets (`gpt-5.5[low]`),
`session/set_model` to change them, a session that may start on a model the
account's plan does not cover, and `codex login status` reporting on stderr.
Record a transcript here the first time a Codex is to hand.
