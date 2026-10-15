//! A read-only MCP server over the open vault, for the agent to search and
//! read notes with, instead of being handed one note up front.
//!
//! HTTP on loopback, because the agent runs inside a sandbox that cannot read
//! nuza's own folder, so a program nuza launched for it could not be reached;
//! a port on 127.0.0.1 can. And "streamable HTTP" in its simplest form: one
//! JSON-RPC message per POST, one JSON answer, no stream, no session state -
//! a little `TcpListener` code rather than a web framework.
//!
//! What makes it safe, each on its own:
//!
//! - **Read-only by construction.** No tool writes; there is no code here that
//!   opens a file for writing.
//! - **Loopback only**, on a port the system picks. Nothing off the machine
//!   reaches it, and a request carrying an `Origin` - a web page - is refused.
//! - **A token per session, for one vault.** nuza is multi-window, so a token
//!   minted for a window on vault A cannot read vault B. Tokens live in memory
//!   and go when the session does.
//! - **Only notes in the vault's index**, found by their path in it, after the
//!   path has been resolved and checked against the vault's root - so `..`
//!   and a symlink out of the vault lead nowhere.
//! - **Capped**: 1 MB requests, 64 KB of headers, 12,000 characters a result.
//!
//! It also cannot help reading every note in the vault, not only the one on
//! screen: that is the point of it, and the reason it can be turned off.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::index::{Note, VaultIndex};
use crate::state::{locked, Vault};

/// What the server is called to the agent, and in its tool calls.
pub(crate) const SERVER_NAME: &str = "nuza";
/// Every tool there is. A guard test holds this list to what is here, so a
/// tool that writes cannot be added without someone noticing.
pub(crate) const TOOL_NAMES: [&str; 7] = [
    "search_vault",
    "read_note",
    "list_notes",
    "outline",
    "backlinks",
    "list_tags",
    "notes_with_tag",
];

/// The tools that change the vault, as opposed to reading it. They are listed
/// and answered only while the person has given the agent full access, and
/// deleting a note moves it to the trash rather than removing it.
pub(crate) const WRITE_TOOL_NAMES: [&str; 5] = [
    "create_note",
    "append_note",
    "edit_note",
    "move_note",
    "delete_note",
];

/// The most text one tool call answers with.
const RESULT_LIMIT: usize = 12_000;
const BODY_LIMIT: usize = 1 << 20;
const HEADER_LIMIT: usize = 64 << 10;
/// How long a connection has to say what it wants.
const READ_PATIENCE: Duration = Duration::from_secs(10);

/// One session's right to read one vault.
pub(crate) struct Grant {
    vault: Arc<Vault>,
    /// The vault's root when the grant was made, resolved. If the window has
    /// since opened another folder, the grant no longer reads anything.
    root: PathBuf,
    /// Paths the agent has been shown - what it may cite.
    given: Mutex<HashSet<String>>,
}

#[derive(Default)]
struct Grants {
    tokens: Mutex<HashMap<String, Arc<Grant>>>,
    /// Whether the agent may change the vault. Set when the person turns full
    /// access on or off, and read when the tools are listed and when one is
    /// called - so a session holding an old list of tools still cannot write.
    writes: AtomicBool,
}

impl Grants {
    fn get(&self, token: &str) -> Option<Arc<Grant>> {
        locked(&self.tokens).get(token).cloned()
    }

    fn writes(&self) -> bool {
        self.writes.load(Ordering::Relaxed)
    }
}

pub(crate) struct VaultServer {
    url: String,
    grants: Arc<Grants>,
    /// The loop taking connections. Stopping it drops the listener, and the
    /// port with it. None for a server the tests ask directly.
    accepting: Option<tokio::task::AbortHandle>,
}

impl VaultServer {
    /// Listens on a free loopback port. Must be called inside the async runtime.
    pub(crate) fn start() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let listener = TcpListener::from_std(listener)?;
        let grants = Arc::<Grants>::default();
        let serving = Arc::clone(&grants);
        let accepting = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let grants = Arc::clone(&serving);
                tokio::spawn(async move { serve(stream, &grants, port).await });
            }
        })
        .abort_handle();
        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            grants,
            accepting: Some(accepting),
        })
    }

    /// Stops listening, and every token with it: the port is closed, and a
    /// request already on its way reads nothing.
    pub(crate) fn stop(&self) {
        locked(&self.grants.tokens).clear();
        if let Some(accepting) = &self.accepting {
            accepting.abort();
        }
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// A token that reads `vault`, as it is open now. None when no folder is.
    pub(crate) fn grant(&self, vault: Arc<Vault>) -> Option<String> {
        let root = locked(&vault.root).clone()?;
        let token = new_token();
        let grant = Grant {
            vault,
            root,
            given: Mutex::default(),
        };
        locked(&self.grants.tokens).insert(token.clone(), Arc::new(grant));
        Some(token)
    }

    /// The token stops reading anything, at once.
    pub(crate) fn revoke(&self, token: &str) {
        locked(&self.grants.tokens).remove(token);
    }

    /// Lets the agent change the vault, or stops it. Read-only until this is
    /// turned on, and read-only again the moment it is turned off.
    pub(crate) fn set_writes(&self, allowed: bool) {
        self.grants.writes.store(allowed, Ordering::Relaxed);
    }

    /// Whether `token` has been shown the note at `path` - so a citation of it
    #[cfg(test)]
    /// is one the agent was given, not one it made up.
    pub(crate) fn was_given(&self, token: &str, path: &str) -> bool {
        self.grants
            .get(token)
            .is_some_and(|grant| locked(&grant.given).contains(path))
    }

    /// Where a citation the agent was given points: the note's own path, and
    /// the line its paragraph starts on. Nothing for a path it was not shown.
    pub(crate) fn locate(
        &self,
        token: &str,
        path: &str,
        paragraph: Option<usize>,
    ) -> Option<(String, usize)> {
        let grant = self.grants.get(token)?;
        if !locked(&grant.given).contains(path) {
            return None;
        }
        let (_, note) = find_note(&grant, path).ok()?;
        let line = match paragraph {
            Some(number) if number > 0 => paragraphs(&note.text)
                .get(number - 1)
                .map_or(1, |(line, _)| *line),
            _ => 1,
        };
        Some((note.path, line))
    }

    /// Every path `token` has been shown, for the panel to link citations to.
    pub(crate) fn given(&self, token: &str) -> Vec<String> {
        self.grants
            .get(token)
            .map(|grant| locked(&grant.given).iter().cloned().collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn grants(&self) -> &Grants {
        &self.grants
    }
}

/// 256 random bits, as hex.
fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system has a source of randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// HTTP, the least of it
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct Response {
    status: u16,
    body: Option<Value>,
}

impl Response {
    fn status(status: u16) -> Self {
        Self { status, body: None }
    }

    fn json(body: Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
        }
    }
}

async fn serve(mut stream: TcpStream, grants: &Grants, port: u16) {
    let response = match tokio::time::timeout(READ_PATIENCE, read_request(&mut stream)).await {
        Ok(Ok(request)) => respond(grants, &request, port),
        Ok(Err(status)) => Response::status(status),
        Err(_) => Response::status(408),
    };
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let body = response
        .body
        .map(|body| serde_json::to_vec(&body).expect("a Value always serializes"))
        .unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if response.status == 405 {
        head.push_str("Allow: POST\r\n");
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

/// Reads one request, within the limits. An error is the status to answer with.
async fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > HEADER_LIMIT {
            return Err(431);
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.map_err(|_| 400u16)?;
        if read == 0 {
            return Err(400);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buffer[..header_end]).map_err(|_| 400u16)?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (
        first.next().unwrap_or_default().to_string(),
        first.next().unwrap_or_default().to_string(),
    );
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.parse().map_err(|_| 400u16))
        .transpose()?
        .unwrap_or(0);
    if length > BODY_LIMIT {
        return Err(413);
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let mut chunk = vec![0u8; (length - body.len()).min(64 << 10)];
        let read = stream.read(&mut chunk).await.map_err(|_| 400u16)?;
        if read == 0 {
            return Err(400);
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// The answer to one request: checked in the order that refuses the most
/// with the least said.
fn respond(grants: &Grants, request: &Request, port: u16) -> Response {
    // A web page reaching for the port, whatever it carries.
    if request.header("origin").is_some() {
        return Response::status(403);
    }
    let host_ok = request.header("host").is_some_and(|host| {
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    });
    if !host_ok {
        return Response::status(403);
    }
    if request.path != "/mcp" {
        return Response::status(404);
    }
    let token = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let Some(grant) = token.and_then(|token| grants.get(token)) else {
        return Response::status(401);
    };
    // The folder the token was for is no longer the window's.
    if locked(&grant.vault.root).as_deref() != Some(grant.root.as_path()) {
        return Response::status(401);
    }
    if request.method != "POST" {
        return Response::status(405);
    }
    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
        return Response::status(400);
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Response::status(400);
    };
    // A notification has nothing to answer.
    let Some(id) = message.get("id").cloned() else {
        return Response::status(202);
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": if grants.writes() {
                "Tools over the writer's notes. Search before reading; cite notes you use. This session can also change them, when asked to: edits go by paragraph, and a deleted note goes to the trash."
            } else {
                "Read-only tools over the writer's notes. Search before reading; cite notes you use."
            },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list(grants.writes()) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            Ok(match call(&grant, name, &arguments, grants.writes()) {
                Ok(text) => {
                    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                }
                Err(problem) => {
                    json!({ "content": [{ "type": "text", "text": problem }], "isError": true })
                }
            })
        }
        _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
    };
    Response::json(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

// ---------------------------------------------------------------------------
// The tools
// ---------------------------------------------------------------------------

/// The tools, described for the model: when to use each, with an example.
///
/// `writes` adds the five that change the vault, for a session the person has
/// given full access. A session that holds a list from before that still
/// cannot use them: `call` asks again.
fn tool_list(writes: bool) -> Value {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required },
        })
    };
    let mut tools = vec![
        tool(
            "search_vault",
            "Search the writer's notes for words. Returns matching lines with the note each is in. Use words likely to appear in the text, e.g. \"beta launch\".",
            json!({
                "query": { "type": "string", "description": "Words to look for." },
                "limit": { "type": "integer", "description": "At most this many lines (default 8, at most 20)." },
            }),
            &["query"],
        ),
        tool(
            "read_note",
            "Read a note, every paragraph numbered ¶1, ¶2… so you can cite one. Long notes come in parts; the reply says where the next starts. Use section to jump to a heading, e.g. {\"path\": \"projects/release.md\", \"section\": \"Timeline\"}.",
            json!({
                "path": { "type": "string", "description": "The note's path in the vault, as other tools give it." },
                "section": { "type": "string", "description": "Start at the heading with this text." },
                "offset": { "type": "integer", "description": "Start at this paragraph number." },
            }),
            &["path"],
        ),
        tool(
            "list_notes",
            "List notes, most recently changed first - to see what there is, or what is in a folder, e.g. {\"folder\": \"projects\"}.",
            json!({
                "folder": { "type": "string", "description": "Only notes in this folder." },
                "limit": { "type": "integer", "description": "At most this many (default 50, at most 200)." },
            }),
            &[],
        ),
        tool(
            "outline",
            "A note's headings, with their paragraph numbers - to decide what to read before reading it.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            "backlinks",
            "The notes that link to a note with [[its name]] - to find what refers to it.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            "list_tags",
            "Every #tag in the vault and how many notes carry it.",
            json!({}),
            &[],
        ),
        tool(
            "notes_with_tag",
            "The notes that carry a #tag, e.g. {\"tag\": \"work\"}.",
            json!({
                "tag": { "type": "string", "description": "The tag, with or without its #." },
                "limit": { "type": "integer", "description": "At most this many (default 50, at most 200)." },
            }),
            &["tag"],
        ),
    ];
    if writes {
        tools.extend([
            tool(
                "create_note",
                "Make a new note. Fails if one is already there, so read first when unsure. New notes go to the trash like any other if deleted.",
                json!({
                    "path": { "type": "string", "description": "Where the note goes, as other tools give paths, e.g. \"projects/launch.md\". Must be inside the vault and end in .md." },
                    "text": { "type": "string", "description": "All of it. Markdown, as it would read in the editor." },
                }),
                &["path", "text"],
            ),
            tool(
                "append_note",
                "Add to the end of a note, leaving what is already there untouched. Use it to jot something down, not to change a sentence - that is edit_note.",
                json!({
                    "path": { "type": "string", "description": "The note's path, as other tools give it." },
                    "text": { "type": "string", "description": "What to add. A blank line is put between it and the note." },
                }),
                &["path", "text"],
            ),
            tool(
                "edit_note",
                "Replace one paragraph of a note, leaving every other paragraph byte for byte as it was. Paragraphs are the ones read_note numbers ¶1, ¶2… so {\"path\": \"projects/launch.md\", \"paragraph\": 3, \"text\": \"…\"} changes only ¶3. Read the note first, and quote the paragraph number you mean - it does not search by text.",
                json!({
                    "path": { "type": "string", "description": "The note's path, as other tools give it." },
                    "paragraph": { "type": "integer", "description": "Which paragraph, as read_note numbered it. A heading is a paragraph too." },
                    "text": { "type": "string", "description": "What the paragraph becomes." },
                }),
                &["path", "paragraph", "text"],
            ),
            tool(
                "move_note",
                "Move or rename a note, e.g. {\"path\": \"launch.md\", \"to\": \"projects/launch.md\"}. Overwrites nothing: it fails if something is already at the new path. [[links]] to the old path are left as they are - say so rather than rewriting other notes.",
                json!({
                    "path": { "type": "string", "description": "The note's path now." },
                    "to": { "type": "string", "description": "Where it should be, inside the vault, ending in .md." },
                }),
                &["path", "to"],
            ),
            tool(
                "delete_note",
                "Move a note to the trash, where the writer can bring it back. Confirm with them first when you chose the note rather than being asked to delete it.",
                json!({
                    "path": { "type": "string", "description": "The note's path, as other tools give it." },
                }),
                &["path"],
            ),
        ]);
    }
    Value::Array(tools)
}

fn text_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn number_arg(arguments: &Value, key: &str, default: usize, most: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .map_or(default, |n| n as usize)
        .clamp(1, most)
}

/// A note's path relative to the vault, with forward slashes.
fn relative(note: &Note, display_root: &Path) -> String {
    let path = Path::new(&note.path);
    path.strip_prefix(display_root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The vault's notes, by their path in it.
fn notes_of(grant: &Grant) -> (Vec<(String, Note)>, PathBuf) {
    let index: &VaultIndex = &grant.vault.index;
    let display_root = index.display_root();
    let notes = index
        .notes()
        .into_iter()
        .map(|note| (relative(&note, &display_root), note))
        .collect();
    (notes, display_root)
}

/// The note at `path`, if `path` names one in the vault.
///
/// The path is taken apart before anything else: nothing absolute, nothing
/// that climbs with `..`. Then the file it names is resolved - symlinks and
/// all - and must still be inside the vault's root, and must be a note the
/// index holds. Any of those failing is the same "no such note".
fn find_note(grant: &Grant, path: &str) -> Result<(String, Note), String> {
    let wanted = Path::new(path.trim().trim_start_matches("./"));
    let missing =
        || format!("There is no note at {path}. Use list_notes or search_vault to find one.");
    if wanted.is_absolute()
        || wanted
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(missing());
    }
    let (notes, display_root) = notes_of(grant);
    let resolved = display_root
        .join(wanted)
        .canonicalize()
        .map_err(|_| missing())?;
    if !resolved.starts_with(&grant.root) {
        return Err(missing());
    }
    let key = wanted
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    notes
        .into_iter()
        .find(|(relative, note)| {
            relative == &key
                && Path::new(&note.path).canonicalize().ok().as_deref() == Some(resolved.as_path())
        })
        .ok_or_else(missing)
}

/// The file a new note goes to, if `path` names a place inside the vault.
///
/// Taken apart first, like `find_note`: nothing absolute, nothing that climbs
/// with `..`, and nothing that is not a `.md`. The folder it goes in must not
/// resolve out of the vault either, so a link inside the vault cannot be used
/// to write somewhere else.
fn new_note_path(grant: &Grant, path: &str) -> Result<PathBuf, String> {
    let wanted = Path::new(path.trim().trim_start_matches("./"));
    let bad =
        || format!("{path} is not a path inside this folder. Give one like \"projects/launch.md\".");
    if wanted.is_absolute()
        || wanted.file_name().is_none()
        || wanted
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(bad());
    }
    if !wanted.to_string_lossy().to_lowercase().ends_with(".md") {
        return Err(format!("A note is a .md file, so {path} is not one."));
    }
    let (_, display_root) = notes_of(grant);
    let folder = wanted.parent().unwrap_or_else(|| Path::new(""));
    if let Ok(resolved) = display_root.join(folder).canonicalize() {
        if !resolved.starts_with(&grant.root) {
            return Err(bad());
        }
    }
    Ok(display_root.join(wanted))
}

/// The note at `path`, for a tool that is about to change it: the same rules
/// as `find_note` for where the path may point, but not the index. A note this
/// session made a moment ago is not in the index yet, and the agent must be
/// able to keep working on what it just wrote.
fn writable_note(grant: &Grant, path: &str) -> Result<(String, PathBuf), String> {
    let full = new_note_path(grant, path)?;
    let key = path
        .trim()
        .trim_start_matches("./")
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if !full.exists() {
        return Err(format!(
            "There is no note at {key}. Use list_notes or search_vault to find one."
        ));
    }
    Ok((key, full))
}

/// A note's paragraphs: runs of non-blank lines, with the line each starts on.
pub(crate) fn paragraphs(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, Vec<&str>)> = None;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            if let Some((start, lines)) = current.take() {
                out.push((start, lines.join("\n")));
            }
        } else {
            current.get_or_insert((index + 1, Vec::new())).1.push(line);
        }
    }
    if let Some((start, lines)) = current {
        out.push((start, lines.join("\n")));
    }
    out
}

/// Cuts `text` to the result limit, and says so when it does.
fn capped(mut text: String) -> String {
    if text.len() <= RESULT_LIMIT {
        return text;
    }
    let mut cut = RESULT_LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str("\n[… cut at 12,000 characters …]");
    text
}

fn heading_text(paragraph: &str) -> Option<&str> {
    let first = paragraph.lines().next()?;
    let text = first.trim_start_matches('#');
    (text.len() < first.len() && text.starts_with(' ')).then(|| text.trim())
}

fn call(grant: &Grant, name: &str, arguments: &Value, writes: bool) -> Result<String, String> {
    let shown = |path: &str| {
        locked(&grant.given).insert(path.to_string());
    };
    match name {
        "search_vault" => {
            let query = text_arg(arguments, "query").ok_or("Give a query to search for.")?;
            let limit = number_arg(arguments, "limit", 8, 20);
            let hits = crate::search::search_index(&grant.vault.index, query, false)?;
            if hits.is_empty() {
                return Ok(format!("Nothing in the notes matches \"{query}\"."));
            }
            // The whole line, not the sidebar's preview of it, which is cut
            // to fit a narrow column.
            let (notes, _) = notes_of(grant);
            let mut out = String::new();
            for hit in hits.into_iter().take(limit) {
                let Some((path, note)) = notes.iter().find(|(_, note)| note.path == hit.path)
                else {
                    continue;
                };
                let line = note
                    .text
                    .lines()
                    .nth(hit.line - 1)
                    .unwrap_or_default()
                    .trim();
                let line: String = line.chars().take(300).collect();
                shown(path);
                out.push_str(&format!("{path}:{}: {line}\n", hit.line));
            }
            Ok(capped(out))
        }
        "read_note" => {
            let path = text_arg(arguments, "path").ok_or("Give the path of the note to read.")?;
            let (relative, note) = find_note(grant, path)?;
            shown(&relative);
            let paragraphs = paragraphs(&note.text);
            let start = match text_arg(arguments, "section") {
                Some(section) => paragraphs
                    .iter()
                    .position(|(_, text)| {
                        heading_text(text).is_some_and(|heading| heading.eq_ignore_ascii_case(section))
                    })
                    .ok_or_else(|| format!("{relative} has no heading \"{section}\". Use outline to see its headings."))?,
                None => number_arg(arguments, "offset", 1, paragraphs.len().max(1)) - 1,
            };
            let mut out = format!("# {relative} ({} paragraphs)\n\n", paragraphs.len());
            let mut next = None;
            for (number, (_, text)) in paragraphs.iter().enumerate().skip(start) {
                let piece = format!("¶{} {text}\n\n", number + 1);
                if out.len() + piece.len() > RESULT_LIMIT - 200 && number > start {
                    next = Some(number + 1);
                    break;
                }
                out.push_str(&piece);
            }
            if let Some(next) = next {
                out.push_str(&format!(
                    "[… continues at ¶{next}: call read_note with offset {next}]"
                ));
            }
            Ok(capped(out))
        }
        "list_notes" => {
            let folder =
                text_arg(arguments, "folder").map(|folder| folder.trim_matches('/').to_string());
            let limit = number_arg(arguments, "limit", 50, 200);
            let (notes, _) = notes_of(grant);
            let mut listed: Vec<(std::time::SystemTime, String, String)> = notes
                .into_iter()
                .filter(|(relative, _)| {
                    folder
                        .as_deref()
                        .is_none_or(|folder| relative.starts_with(&format!("{folder}/")))
                })
                .map(|(relative, note)| {
                    let modified = std::fs::metadata(&note.path)
                        .and_then(|meta| meta.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    let title = note
                        .text
                        .lines()
                        .find_map(|line| line.strip_prefix("# "))
                        .map(str::trim)
                        .unwrap_or_else(|| note.name.trim_end_matches(".md"))
                        .to_string();
                    (modified, relative, title)
                })
                .collect();
            listed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            if listed.is_empty() {
                return Ok("No notes there.".to_string());
            }
            let mut out = String::new();
            for (_, relative, title) in listed.into_iter().take(limit) {
                shown(&relative);
                out.push_str(&format!("{relative} — {title}\n"));
            }
            Ok(capped(out))
        }
        "outline" => {
            let path = text_arg(arguments, "path").ok_or("Give the path of the note.")?;
            let (relative, note) = find_note(grant, path)?;
            shown(&relative);
            let headings: Vec<String> = paragraphs(&note.text)
                .iter()
                .enumerate()
                .filter_map(|(number, (_, text))| {
                    let first = text.lines().next()?;
                    heading_text(text).map(|_| format!("¶{} {first}", number + 1))
                })
                .collect();
            Ok(if headings.is_empty() {
                format!("{relative} has no headings.")
            } else {
                capped(headings.join("\n"))
            })
        }
        "backlinks" => {
            let path = text_arg(arguments, "path").ok_or("Give the path of the note.")?;
            let (relative, _) = find_note(grant, path)?;
            let stem = relative.trim_end_matches(".md").to_lowercase();
            let name = stem.rsplit('/').next().unwrap_or(&stem).to_string();
            let display_root = grant.vault.index.display_root();
            let links = crate::wiki::scan_index(&grant.vault.index, crate::wiki::wiki_links_in);
            let mut out = String::new();
            for link in links {
                let target = link
                    .target
                    .split(['|', '#'])
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                let target = target.trim_end_matches(".md");
                if target != stem && target != name {
                    continue;
                }
                let from = Path::new(&link.from)
                    .strip_prefix(&display_root)
                    .map(|rest| rest.to_string_lossy().replace('\\', "/"))
                    .unwrap_or(link.from.clone());
                shown(&from);
                out.push_str(&format!("{from}:{}: {}\n", link.line, link.preview.trim()));
            }
            Ok(if out.is_empty() {
                format!("Nothing links to {relative}.")
            } else {
                capped(out)
            })
        }
        "list_tags" => {
            let mut counts: HashMap<String, HashSet<String>> = HashMap::new();
            for tag in crate::wiki::scan_index(&grant.vault.index, crate::tags::tags_in) {
                counts
                    .entry(tag.tag.to_lowercase())
                    .or_default()
                    .insert(tag.from);
            }
            if counts.is_empty() {
                return Ok("No #tags in the notes.".to_string());
            }
            let mut tags: Vec<(String, usize)> = counts
                .into_iter()
                .map(|(tag, notes)| (tag, notes.len()))
                .collect();
            tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Ok(capped(
                tags.iter()
                    .map(|(tag, count)| format!("#{tag} ({count})"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        }
        "notes_with_tag" => {
            let tag = text_arg(arguments, "tag")
                .ok_or("Give a tag.")?
                .trim_start_matches('#')
                .to_lowercase();
            let limit = number_arg(arguments, "limit", 50, 200);
            let display_root = grant.vault.index.display_root();
            let mut found: Vec<String> =
                crate::wiki::scan_index(&grant.vault.index, crate::tags::tags_in)
                    .into_iter()
                    .filter(|found| found.tag.to_lowercase() == tag)
                    .map(|found| {
                        Path::new(&found.from)
                            .strip_prefix(&display_root)
                            .map(|rest| rest.to_string_lossy().replace('\\', "/"))
                            .unwrap_or(found.from)
                    })
                    .collect();
            found.sort();
            found.dedup();
            if found.is_empty() {
                return Ok(format!("No notes carry #{tag}."));
            }
            for path in found.iter().take(limit) {
                shown(path);
            }
            Ok(capped(
                found.into_iter().take(limit).collect::<Vec<_>>().join("\n"),
            ))
        }
        "create_note" if writes => {
            let path = text_arg(arguments, "path").ok_or("Give the path the note should go at.")?;
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .ok_or("Give the note's text.")?;
            let full = new_note_path(grant, path)?;
            if full.exists() {
                return Err(format!(
                    "There is already a note at {path}. Read it, or change it with edit_note."
                ));
            }
            if let Some(folder) = full.parent() {
                std::fs::create_dir_all(folder)
                    .map_err(|error| format!("Could not make {}: {error}", folder.display()))?;
            }
            crate::files::write_atomically(&full, text.as_bytes())
                .map_err(|error| format!("Could not write {path}: {error}"))?;
            shown(path);
            Ok(format!("Made {path}."))
        }
        "append_note" if writes => {
            let path = text_arg(arguments, "path").ok_or("Give the note's path.")?;
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .ok_or("Give the text to add.")?;
            let (key, file) = writable_note(grant, path)?;
            let mut whole = std::fs::read_to_string(&file)
                .map_err(|error| format!("Could not read {key}: {error}"))?;
            if !whole.is_empty() && !whole.ends_with('\n') {
                whole.push('\n');
            }
            if !whole.trim().is_empty() {
                whole.push('\n');
            }
            whole.push_str(text.trim_end());
            whole.push('\n');
            crate::files::write_atomically(&file, whole.as_bytes())
                .map_err(|error| format!("Could not write {key}: {error}"))?;
            shown(&key);
            Ok(format!("Added to {key}."))
        }
        "edit_note" if writes => {
            let path = text_arg(arguments, "path").ok_or("Give the note's path.")?;
            let which = arguments
                .get("paragraph")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or("Give the paragraph number read_note showed you, like 3.")?;
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .ok_or("Give what the paragraph becomes.")?;
            let (key, file) = writable_note(grant, path)?;
            let whole = std::fs::read_to_string(&file)
                .map_err(|error| format!("Could not read {key}: {error}"))?;
            let found = paragraphs(&whole);
            let at = which as usize;
            let (start, current) = found.get(at - 1).ok_or_else(|| {
                format!("{key} has {} paragraphs, so there is no ¶{at}.", found.len())
            })?;
            let mut lines: Vec<&str> = whole.lines().collect();
            let ends_with_newline = whole.ends_with('\n');
            lines.splice(
                start - 1..start - 1 + current.lines().count(),
                text.lines(),
            );
            let mut out = lines.join("\n");
            if ends_with_newline {
                out.push('\n');
            }
            crate::files::write_atomically(&file, out.as_bytes())
                .map_err(|error| format!("Could not write {key}: {error}"))?;
            shown(&key);
            Ok(format!(
                "¶{at} of {key} was:\n\n{}\n\nIt is now:\n\n{}",
                current.trim(),
                text.trim()
            ))
        }
        "move_note" if writes => {
            let path = text_arg(arguments, "path").ok_or("Give the note's path.")?;
            let to = text_arg(arguments, "to").ok_or("Give where it should go.")?;
            let (key, file) = writable_note(grant, path)?;
            let full = new_note_path(grant, to)?;
            if full.exists() {
                return Err(format!("There is already something at {to}."));
            }
            if let Some(folder) = full.parent() {
                std::fs::create_dir_all(folder)
                    .map_err(|error| format!("Could not make {}: {error}", folder.display()))?;
            }
            std::fs::rename(&file, &full)
                .map_err(|error| format!("Could not move {key} to {to}: {error}"))?;
            shown(to);
            Ok(format!(
                "Moved {key} to {to}. Links to the old name were left alone."
            ))
        }
        "delete_note" if writes => {
            let path = text_arg(arguments, "path").ok_or("Give the note's path.")?;
            let (key, file) = writable_note(grant, path)?;
            trash::delete(&file)
                .map_err(|error| format!("Could not move {key} to the trash: {error}"))?;
            Ok(format!("Moved {key} to the trash."))
        }
        // A session that was given a list of tools before full access was
        // turned off can still ask for one of these. Say why, rather than
        // "no such tool", which would read as a mistake in its request.
        _ if WRITE_TOOL_NAMES.contains(&name) => Err(format!(
            "{name} changes the notes, and this session was not given full access. \
             The writer turns it on in Settings, under AI."
        )),
        _ => Err(format!("There is no tool called {name}.")),
    }
}

#[cfg(test)]
mod tests;
