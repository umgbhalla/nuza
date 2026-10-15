//! The vault server against real folders on disk: what it answers, and
//! everything it refuses.

use super::*;
use crate::index::VaultIndex;
use std::fs;

/// A vault on disk, opened the way a window opens one.
struct Opened {
    _dir: tempfile::TempDir,
    root: PathBuf,
    vault: Arc<Vault>,
}

fn open(files: &[(&str, &str)]) -> Opened {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (path, text) in files {
        let at = root.join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(at, text).unwrap();
    }
    let vault = Arc::new(Vault {
        root: Mutex::new(Some(root.clone())),
        index: VaultIndex::for_folder(&root).unwrap(),
        ..Vault::default()
    });
    Opened {
        _dir: dir,
        root,
        vault,
    }
}

const PORT: u16 = 4321;

fn request(token: Option<&str>, body: Value) -> Request {
    let mut headers = vec![("Host".to_string(), format!("127.0.0.1:{PORT}"))];
    if let Some(token) = token {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    Request {
        method: "POST".into(),
        path: "/mcp".into(),
        headers,
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn tool_call(name: &str, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
}

/// A server without its socket, for asking it things directly.
fn server() -> VaultServer {
    VaultServer {
        url: format!("http://127.0.0.1:{PORT}/mcp"),
        grants: Arc::default(),
        accepting: None,
    }
}

/// What a tool said, and whether it said it as an error.
fn ask(server: &VaultServer, token: &str, name: &str, arguments: Value) -> (String, bool) {
    let response = respond(
        server.grants(),
        &request(Some(token), tool_call(name, arguments)),
        PORT,
    );
    assert_eq!(response.status, 200);
    let body = response.body.unwrap();
    let result = &body["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"].as_bool().unwrap(),
    )
}

#[test]
fn every_tool_is_listed_with_a_description_and_a_schema() {
    let opened = open(&[]);
    let server = server();
    let token = server.grant(Arc::clone(&opened.vault)).unwrap();
    let response = respond(
        server.grants(),
        &request(
            Some(&token),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        ),
        PORT,
    );
    let tools = response.body.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, TOOL_NAMES);
    for tool in &tools {
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|text| text.len() > 20),
            "{tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }
}

#[test]
fn no_reading_tool_sounds_like_a_write_and_every_writing_tool_says_what_it_does() {
    // A guard in two halves, so neither list can drift quietly: nothing the
    // agent only reads is named like a change, and nothing it changes is
    // named vaguely enough to hide among them.
    for name in TOOL_NAMES {
        for verb in [
            "write", "edit", "save", "delete", "create", "move", "rename", "append", "update",
        ] {
            assert!(
                !name.contains(verb),
                "{name} sounds like it changes something"
            );
        }
    }
    for name in WRITE_TOOL_NAMES {
        assert!(
            ["create", "append", "edit", "move", "delete"]
                .iter()
                .any(|verb| name.contains(verb)),
            "{name} does not say what it does"
        );
    }
    // A name that is no tool at all is still no tool, and says so.
    let opened = open(&[]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let (said, error) = ask(
        &server,
        &token,
        "write_note",
        json!({ "path": "a.md", "text": "x" }),
    );
    assert!(error && said.contains("no tool"), "{said}");
}

#[test]
fn the_writing_tools_are_listed_only_once_full_access_is_on() {
    let opened = open(&[]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let listed = |server: &VaultServer, token: &str| -> Vec<String> {
        let response = respond(
            server.grants(),
            &request(
                Some(token),
                json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            ),
            PORT,
        );
        response.body.unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(listed(&server, &token), TOOL_NAMES.to_vec());
    server.set_writes(true);
    let with_writes = listed(&server, &token);
    for name in TOOL_NAMES {
        assert!(with_writes.contains(&name.to_string()), "{name} went missing");
    }
    for name in WRITE_TOOL_NAMES {
        assert!(
            with_writes.contains(&name.to_string()),
            "{name} was not listed"
        );
    }
    // And they go away again the moment it is turned off, even for a session
    // that already holds the long list.
    server.set_writes(false);
    assert_eq!(listed(&server, &token), TOOL_NAMES.to_vec());
}

#[test]
fn nothing_can_be_changed_until_full_access_is_turned_on() {
    let opened = open(&[("a.md", "one\n\ntwo\n")]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    for name in WRITE_TOOL_NAMES {
        let (said, error) = ask(
            &server,
            &token,
            name,
            json!({ "path": "a.md", "text": "x", "paragraph": 1, "to": "b.md" }),
        );
        assert!(error && said.contains("full access"), "{name}: {said}");
    }
    assert_eq!(
        fs::read_to_string(opened.root.join("a.md")).unwrap(),
        "one\n\ntwo\n"
    );
    assert!(!opened.root.join("b.md").exists());
}

#[test]
fn with_full_access_a_note_can_be_made_changed_moved_and_deleted() {
    let opened = open(&[("a.md", "one\n\ntwo\n")]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    server.set_writes(true);

    let (said, error) = ask(
        &server,
        &token,
        "create_note",
        json!({ "path": "projects/launch.md", "text": "# Launch\n" }),
    );
    assert!(!error, "{said}");
    assert_eq!(
        fs::read_to_string(opened.root.join("projects/launch.md")).unwrap(),
        "# Launch\n"
    );

    // A second note at the same path is refused, not written over.
    let (said, error) = ask(
        &server,
        &token,
        "create_note",
        json!({ "path": "projects/launch.md", "text": "gone\n" }),
    );
    assert!(error && said.contains("already a note"), "{said}");
    assert_eq!(
        fs::read_to_string(opened.root.join("projects/launch.md")).unwrap(),
        "# Launch\n"
    );

    let (said, error) = ask(
        &server,
        &token,
        "append_note",
        json!({ "path": "projects/launch.md", "text": "Timeline." }),
    );
    assert!(!error, "{said}");
    assert_eq!(
        fs::read_to_string(opened.root.join("projects/launch.md")).unwrap(),
        "# Launch\n\nTimeline.\n"
    );

    // One paragraph changes; the one above it does not, and the answer says
    // both so the writer can see what moved.
    let (said, error) = ask(
        &server,
        &token,
        "edit_note",
        json!({ "path": "a.md", "paragraph": 2, "text": "TWO" }),
    );
    assert!(!error, "{said}");
    assert!(said.contains("two") && said.contains("TWO"), "{said}");
    assert_eq!(
        fs::read_to_string(opened.root.join("a.md")).unwrap(),
        "one\n\nTWO\n"
    );
    // There is no ¶9 in a two-paragraph note.
    let (said, error) = ask(
        &server,
        &token,
        "edit_note",
        json!({ "path": "a.md", "paragraph": 9, "text": "x" }),
    );
    assert!(error && said.contains("no ¶9"), "{said}");

    let (said, error) = ask(
        &server,
        &token,
        "move_note",
        json!({ "path": "a.md", "to": "archive/a.md" }),
    );
    assert!(!error, "{said}");
    assert!(opened.root.join("archive/a.md").exists());
    assert!(!opened.root.join("a.md").exists());
    // Moving onto something is refused, never an overwrite.
    let (said, error) = ask(
        &server,
        &token,
        "move_note",
        json!({ "path": "projects/launch.md", "to": "archive/a.md" }),
    );
    assert!(error && said.contains("already something"), "{said}");

    // Delete moves it to the trash, so the file is gone from the vault.
    let (said, error) = ask(&server, &token, "delete_note", json!({ "path": "archive/a.md" }));
    assert!(!error, "{said}");
    assert!(said.contains("trash"), "{said}");
    assert!(!opened.root.join("archive/a.md").exists());
}

#[test]
fn a_writing_tool_cannot_reach_outside_the_vault() {
    let opened = open(&[("a.md", "one\n")]);
    let outside = opened.root.parent().unwrap().join("outside.md");
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    server.set_writes(true);
    for path in [
        "../outside.md",
        "/tmp/outside.md",
        "notes/../../outside.md",
        "a.txt",
        "",
    ] {
        let (said, error) = ask(
            &server,
            &token,
            "create_note",
            json!({ "path": path, "text": "x" }),
        );
        assert!(error, "{path} was accepted: {said}");
    }
    assert!(!outside.exists());
    // And a note that is not there is not a note to change.
    let (said, error) = ask(&server, &token, "delete_note", json!({ "path": "missing.md" }));
    assert!(error && said.contains("no note at"), "{said}");
}

#[test]
fn without_the_right_token_nothing_is_answered() {
    let opened = open(&[("a.md", "hello")]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let body = tool_call("read_note", json!({ "path": "a.md" }));

    assert_eq!(
        respond(server.grants(), &request(None, body.clone()), PORT).status,
        401
    );
    assert_eq!(
        respond(
            server.grants(),
            &request(Some("not-the-token"), body.clone()),
            PORT
        )
        .status,
        401
    );
    let mut wrong_scheme = request(None, body.clone());
    wrong_scheme
        .headers
        .push(("Authorization".into(), format!("Basic {token}")));
    assert_eq!(respond(server.grants(), &wrong_scheme, PORT).status, 401);

    server.revoke(&token);
    assert_eq!(
        respond(server.grants(), &request(Some(&token), body), PORT).status,
        401
    );
}

#[test]
fn a_web_page_and_a_strange_host_are_turned_away() {
    let opened = open(&[("a.md", "hello")]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let mut from_a_page = request(Some(&token), tool_call("list_notes", json!({})));
    from_a_page
        .headers
        .push(("Origin".into(), "https://example.com".into()));
    assert_eq!(respond(server.grants(), &from_a_page, PORT).status, 403);

    let mut rebound = request(Some(&token), tool_call("list_notes", json!({})));
    rebound.headers[0].1 = format!("attacker.example:{PORT}");
    assert_eq!(respond(server.grants(), &rebound, PORT).status, 403);
}

#[test]
fn only_post_to_the_one_path_is_answered() {
    let opened = open(&[]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let mut get = request(Some(&token), json!({}));
    get.method = "GET".into();
    assert_eq!(respond(server.grants(), &get, PORT).status, 405);
    let mut elsewhere = request(Some(&token), json!({}));
    elsewhere.path = "/".into();
    assert_eq!(respond(server.grants(), &elsewhere, PORT).status, 404);
    // A notification is taken, and there is nothing to say back.
    let notified = respond(
        server.grants(),
        &request(
            Some(&token),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        ),
        PORT,
    );
    assert_eq!(notified.status, 202);
}

#[test]
fn a_path_out_of_the_vault_is_no_note_at_all() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), "the-secret-words").unwrap();
    let opened = open(&[("notes/a.md", "hello")]);
    // A symlink inside the vault, pointing out of it.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        opened.root.join("notes/link.md"),
    )
    .unwrap();

    let server = server();
    let token = server.grant(Arc::clone(&opened.vault)).unwrap();
    let escape = format!(
        "../{}/secret.md",
        outside.path().file_name().unwrap().to_string_lossy()
    );
    let absolute = outside
        .path()
        .join("secret.md")
        .to_string_lossy()
        .into_owned();
    for path in [
        escape.as_str(),
        absolute.as_str(),
        "notes/../../secret.md",
        "notes/link.md",
        "/etc/hosts",
    ] {
        let (said, error) = ask(&server, &token, "read_note", json!({ "path": path }));
        assert!(error, "{path} was read: {said}");
        assert!(!said.contains("the-secret-words"));
    }
    let (said, error) = ask(
        &server,
        &token,
        "read_note",
        json!({ "path": "notes/a.md" }),
    );
    assert!(!error && said.contains("hello"), "{said}");
}

#[test]
fn one_sessions_token_cannot_read_another_sessions_vault() {
    let work = open(&[("plan.md", "work-only-words")]);
    let home = open(&[("diary.md", "home-only-words")]);
    let server = server();
    let work_token = server.grant(Arc::clone(&work.vault)).unwrap();
    let home_token = server.grant(Arc::clone(&home.vault)).unwrap();
    assert_ne!(work_token, home_token);

    let (said, _) = ask(
        &server,
        &home_token,
        "search_vault",
        json!({ "query": "work-only-words" }),
    );
    assert!(!said.contains("plan.md"), "{said}");
    let (said, error) = ask(
        &server,
        &home_token,
        "read_note",
        json!({ "path": "plan.md" }),
    );
    assert!(error && !said.contains("work-only-words"), "{said}");
    let (said, _) = ask(
        &server,
        &work_token,
        "read_note",
        json!({ "path": "plan.md" }),
    );
    assert!(said.contains("work-only-words"));
}

#[test]
fn a_token_stops_working_when_its_window_opens_another_folder() {
    let opened = open(&[("a.md", "hello")]);
    let server = server();
    let token = server.grant(Arc::clone(&opened.vault)).unwrap();
    *locked(&opened.vault.root) = Some(PathBuf::from("/somewhere/else"));
    let response = respond(
        server.grants(),
        &request(Some(&token), tool_call("list_notes", json!({}))),
        PORT,
    );
    assert_eq!(response.status, 401);
}

#[test]
fn read_note_numbers_paragraphs_and_says_where_a_long_one_continues() {
    let mut long = String::from("# Long\n\n");
    for n in 1..=400 {
        long.push_str(&format!(
            "Paragraph {n} with some words in it to take up room.\n\n"
        ));
    }
    let opened = open(&[("long.md", &long)]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();

    let (first, _) = ask(&server, &token, "read_note", json!({ "path": "long.md" }));
    assert!(first.starts_with("# long.md (401 paragraphs)"), "{first}");
    assert!(first.contains("¶1 # Long"));
    assert!(first.contains("¶2 Paragraph 1 with"));
    assert!(first.len() <= RESULT_LIMIT);
    let next: usize = first
        .rsplit("offset ")
        .next()
        .and_then(|rest| rest.trim_end_matches(']').parse().ok())
        .expect("says where it continues");

    let (second, _) = ask(
        &server,
        &token,
        "read_note",
        json!({ "path": "long.md", "offset": next }),
    );
    assert!(
        second.contains(&format!("¶{next} Paragraph {}", next - 1)),
        "{second}"
    );
}

#[test]
fn read_note_jumps_to_a_section() {
    let opened = open(&[(
        "plan.md",
        "# Plan\n\nIntro.\n\n## Timeline\n\nBeta in March.\n",
    )]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let (said, _) = ask(
        &server,
        &token,
        "read_note",
        json!({ "path": "plan.md", "section": "timeline" }),
    );
    assert!(
        said.contains("¶3 ## Timeline") && !said.contains("Intro."),
        "{said}"
    );
    let (outline, _) = ask(&server, &token, "outline", json!({ "path": "plan.md" }));
    assert_eq!(outline, "¶1 # Plan\n¶3 ## Timeline");
}

#[test]
fn search_finds_a_phrase_and_nothing_in_an_ignored_folder() {
    let opened = open(&[
        ("projects/release.md", "Beta in March, GA in May."),
        (".hidden/old.md", "Beta in March, GA in May."),
        ("node_modules/pkg/readme.md", "Beta in March, GA in May."),
    ]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let (said, error) = ask(
        &server,
        &token,
        "search_vault",
        json!({ "query": "GA in May" }),
    );
    assert!(!error);
    assert_eq!(
        said.trim(),
        "projects/release.md:1: Beta in March, GA in May."
    );
    // What it was shown, it may cite.
    assert!(server.was_given(&token, "projects/release.md"));
    assert!(!server.was_given(&token, "elsewhere.md"));
}

#[test]
fn a_result_past_the_limit_is_cut_and_says_so() {
    let opened = open(&[]);
    let server = server();
    let token = server.grant(Arc::clone(&opened.vault)).unwrap();
    let names: Vec<String> = (0..300)
        .map(|n| format!("a-rather-long-note-name-number-{n:03}.md"))
        .collect();
    for name in &names {
        fs::write(opened.root.join(name), "x").unwrap();
    }
    // A fresh index, now the notes are there.
    let reopened = Arc::new(Vault {
        root: Mutex::new(Some(opened.root.clone())),
        index: VaultIndex::for_folder(&opened.root).unwrap(),
        ..Vault::default()
    });
    let token_two = server.grant(reopened).unwrap();
    let _ = token;
    let (said, _) = ask(&server, &token_two, "list_notes", json!({ "limit": 200 }));
    assert!(said.len() <= RESULT_LIMIT + 40);
    assert!(
        said.ends_with("[… cut at 12,000 characters …]"),
        "{}",
        &said[said.len() - 60..]
    );
}

#[test]
fn tags_and_backlinks_are_read_from_the_notes() {
    let opened = open(&[
        ("plan.md", "# Plan\n#work #release\n"),
        ("notes/standup.md", "Talked about [[plan]] and #work.\n"),
        ("other.md", "Nothing here.\n"),
    ]);
    let server = server();
    let token = server.grant(opened.vault).unwrap();
    let (tags, _) = ask(&server, &token, "list_tags", json!({}));
    assert_eq!(tags, "#work (2)\n#release (1)");
    let (tagged, _) = ask(&server, &token, "notes_with_tag", json!({ "tag": "#work" }));
    assert_eq!(tagged, "notes/standup.md\nplan.md");
    let (links, _) = ask(&server, &token, "backlinks", json!({ "path": "plan.md" }));
    assert_eq!(
        links.trim(),
        "notes/standup.md:1: Talked about [[plan]] and #work."
    );
}

#[tokio::test]
async fn it_answers_over_a_real_socket_on_loopback() {
    let opened = open(&[("a.md", "over the wire")]);
    let server = VaultServer::start().unwrap();
    assert!(server.url().starts_with("http://127.0.0.1:"));
    let token = server.grant(opened.vault).unwrap();
    let port: u16 = server
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();

    let body = serde_json::to_vec(&tool_call("read_note", json!({ "path": "a.md" }))).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.contains("over the wire"), "{answer}");
}

#[tokio::test]
async fn stopped_it_closes_its_port_and_its_tokens_read_nothing() {
    let opened = open(&[("a.md", "words")]);
    let server = VaultServer::start().unwrap();
    let token = server.grant(opened.vault).unwrap();
    let port: u16 = server
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());

    server.stop();
    // The listener goes when the loop holding it is next polled.
    let mut closed = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            closed = true;
            break;
        }
    }
    assert!(closed, "the port is still open");
    assert!(server.grants().get(&token).is_none());
}
