use serde_json::{Value, json};

use crate::help;
use crate::server::{Server, Tool};

const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST: &str = "2025-06-18";

/// Longest path an argument may carry, in bytes. Past it the OS refuses the name anyway.
const PATH_MAX: usize = 4096;

pub fn handle(server: &mut Server, msg: Value) -> Option<Value> {
    // MCP 2025-06-18 dropped JSON-RPC batching: an array is refused whole, empty or not.
    if msg.is_array() {
        return Some(error(Value::Null, -32600, "batches are not supported"));
    }
    if !msg.is_object() {
        return Some(error(Value::Null, -32600, "invalid request: expected a JSON object"));
    }
    // No id means a notification: it gets no reply, whatever it asks for.
    let id = msg.get("id")?.clone();
    // A request id is a string, a number or null (JSON-RPC 2.0); anything else cannot be echoed back.
    if !matches!(id, Value::Null | Value::String(_) | Value::Number(_)) {
        return Some(error(Value::Null, -32600, "invalid request: id must be a string, a number or null"));
    }
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match msg.get("method").and_then(Value::as_str) {
        Some("initialize") => Ok(initialize(&params)),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(tools_list()),
        Some("tools/call") => Ok(tools_call(server, &params)),
        Some("resources/list") => Ok(resources_list()),
        Some("resources/read") => resources_read(server, &params),
        Some(m) => Err((-32601, format!("method not found: {m}"))),
        None => Err((-32600, "invalid request: no method".to_owned())),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => error(id, code, &message),
    })
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Every tool the server answers, `help` included.
fn all_tools() -> Vec<Tool> {
    help::catalogue()
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = if VERSIONS.contains(&asked) { asked } else { LATEST };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}, "resources": {}},
        "serverInfo": {"name": "edith", "version": env!("CARGO_PKG_VERSION")},
        "instructions": help::short(&all_tools()),
    })
}

fn tools_list() -> Value {
    let tools: Vec<Value> = all_tools()
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
        .collect();
    json!({"tools": tools})
}

fn resources_list() -> Value {
    json!({"resources": [
        {
            "uri": "edith://help",
            "name": "help",
            "title": "edith cheat sheet",
            "description": "Addresses, times, undo, the tool list, edit ops and recipes.",
            "mimeType": "text/markdown"
        },
        {
            "uri": "edith://project",
            "name": "project",
            "title": "project status",
            "description": "The project_status text: path, duration, lanes, sources, undo depth.",
            "mimeType": "text/plain"
        },
        {
            "uri": "edith://timeline",
            "name": "timeline",
            "title": "timeline",
            "description": "The timeline text: every lane's clips at their addresses.",
            "mimeType": "text/plain"
        }
    ]})
}

fn resources_read(server: &mut Server, params: &Value) -> Result<Value, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or((-32602, "resources/read needs a `uri`".to_owned()))?;
    let (mime, text) = match uri {
        "edith://help" => ("text/markdown", help::sheet(&all_tools())),
        "edith://project" => ("text/plain", run_named(server, "project_status")),
        "edith://timeline" => ("text/plain", run_named(server, "timeline")),
        other => return Err((-32002, format!("resource not found: {other}"))),
    };
    Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
}

/// Runs a tool with no arguments and returns its text, which is the error text when it refuses.
fn run_named(server: &mut Server, name: &str) -> String {
    let tools = all_tools();
    match tools.iter().find(|t| t.name == name) {
        Some(tool) => (tool.run)(server, &json!({})).text,
        None => format!("tool `{name}` is not registered"),
    }
}

fn tools_call(server: &mut Server, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    };
    let tools = all_tools();
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        return refusal(help::unknown_tool(&tools, name));
    };
    if let Err(text) = check_args(tool, &args) {
        return refusal(text);
    }
    // One tool call is one undo step: every engine edit it makes shares a single snapshot.
    let mark = server.session.as_mut().map(|s| s.step_mark());
    if let Some(s) = server.session.as_mut() {
        s.begin_batch();
    }
    // `sequence` points a call at another sequence: a read goes back to the active one after.
    let targeted = tool.name != "sequence";
    let reply = match if targeted { crate::tools::sequence::enter(server, &args, tool.name != "edit") } else { Ok(None) } {
        Ok(back) => {
            let reply = (tool.run)(server, &args);
            crate::tools::sequence::leave(server, back);
            reply
        }
        Err(refused) => refused,
    };
    if let Some(s) = server.session.as_mut() {
        s.end_batch();
        // An edit's step names its ops, so the GUI's Undo says what the call did.
        if let (Some(mark), Some(label)) = (mark, edit_step_label(tool.name, &args)) {
            s.name_step(mark, &label);
        }
    }
    let mut content = vec![json!({"type": "text", "text": reply.text})];
    for (mime, data) in &reply.images {
        content.push(json!({"type": "image", "data": data, "mimeType": mime}));
    }
    let mut out = json!({"content": content, "isError": reply.is_error});
    if let Some(s) = reply.structured {
        out["structuredContent"] = *s;
    }
    out
}

/// The name an edit call's step carries: its ops in order, each named once, as far as
/// the menu row has room for. `None` for any other tool, and for an edit with no ops.
fn edit_step_label(tool: &str, args: &Value) -> Option<String> {
    if tool != "edit" {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for op in args.get("ops").and_then(Value::as_array)? {
        let Some(name) = op.get("op").and_then(Value::as_str) else {
            continue;
        };
        let name = name.replace('_', " ");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut label = String::new();
    for name in names {
        let next = match label.is_empty() {
            true => name,
            false => format!("{label}, {name}"),
        };
        if next.chars().count() >= 30 {
            break;
        }
        label = next;
    }
    let mut chars = label.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

fn refusal(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// Refuses a call whose top-level arguments do not fit the tool's `inputSchema`:
/// a key the schema does not list, a `required` key that is missing, or a value
/// whose JSON type differs from a single `type` the schema states. A null value
/// counts as absent, as the tools read it. Nested values (the ops of `edit`) are
/// left to the tool that reads them.
pub fn check_args(tool: &Tool, args: &Value) -> Result<(), String> {
    let Some(given) = args.as_object() else {
        return Err(format!("arguments for {} must be a JSON object", tool.name));
    };
    let schema = (tool.schema)();
    let empty = serde_json::Map::new();
    let props = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);

    for key in given.keys() {
        if !props.contains_key(key) {
            let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
            let accepted = if accepted.is_empty() { "none".to_owned() } else { accepted.join(", ") };
            return Err(format!("unknown argument `{key}` for {}; accepted: {accepted}", tool.name));
        }
    }
    let required = schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    for key in required {
        if given.get(key).is_none_or(Value::is_null) {
            let hint = prop_sentence(props.get(key)).map(|d| format!(": {d}")).unwrap_or_default();
            return Err(format!("missing required argument `{key}` for {}{hint}", tool.name));
        }
    }
    for (key, value) in given {
        if value.is_null() {
            continue;
        }
        if is_path_key(key) {
            let longest = match value {
                Value::String(s) => s.len(),
                Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::len).max().unwrap_or(0),
                _ => 0,
            };
            if longest > PATH_MAX {
                return Err(format!(
                    "argument `{key}` for {} has a path of {longest} bytes; a path is at most {PATH_MAX} bytes",
                    tool.name
                ));
            }
        }
        if let (Some(max), Some(text)) = (
            props.get(key).and_then(|p| p.get("maxLength")).and_then(Value::as_u64),
            value.as_str(),
        ) {
            let n = text.chars().count();
            if n as u64 > max {
                return Err(format!(
                    "argument `{key}` for {} must be at most {max} characters, got {n}",
                    tool.name
                ));
            }
        }
        let Some(prop) = props.get(key) else {
            continue;
        };
        let types = declared_types(prop);
        if types.is_empty() || types.iter().any(|ty| type_matches(ty, value)) {
            continue;
        }
        // A number that fails an integer type is named by its value, so `1.5` reads as
        // `got 1.5` rather than `got number`.
        let got = match value {
            Value::Number(n) if types.iter().any(|ty| ty == "integer") => n.to_string(),
            _ => json_type(value).to_owned(),
        };
        let hint = prop_sentence(Some(prop)).map(|d| format!(": {d}")).unwrap_or_default();
        return Err(format!(
            "argument `{key}` for {} must be {}, got {got}; for example {}{hint}",
            tool.name,
            describe_any(&types),
            help::example(key, prop)
        ));
    }
    Ok(())
}

/// The first sentence of a property's `description`, so a refusal can say what the argument means.
/// A period ends a sentence unless it follows an abbreviation (`e.g.`, `i.e.`).
fn prop_sentence(prop: Option<&Value>) -> Option<String> {
    let text = prop?.get("description")?.as_str()?.trim();
    let mut end = text.len();
    for (i, _) in text.match_indices(". ") {
        let before = text[..i].to_ascii_lowercase();
        if !(before.ends_with("e.g") || before.ends_with("i.e")) {
            end = i + 1;
            break;
        }
    }
    let sentence = text[..end].trim();
    (!sentence.is_empty()).then(|| sentence.to_owned())
}

/// The JSON type names a schema property states: its `type`, one name or a list of them.
fn declared_types(prop: &Value) -> Vec<String> {
    match prop.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// Whether an argument named `key` holds a filesystem path, or a list of them.
fn is_path_key(key: &str) -> bool {
    key.contains("path") || key.contains("file") || key == "out" || key.starts_with("out_") || key == "glob" || key == "dir" || key == "folder"
}

fn type_matches(ty: &str, v: &Value) -> bool {
    match ty {
        "string" => v.is_string(),
        "number" => v.is_number(),
        // An integral float such as `1000000000.0` is a whole number too; the tool then
        // says whether it is in range.
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

/// The types a refusal names: `an array or a string` for `["array", "string"]`.
fn describe_any(types: &[String]) -> String {
    types.iter().map(|ty| describe(ty)).collect::<Vec<_>>().join(" or ")
}

fn describe(ty: &str) -> String {
    match ty {
        "integer" => "an integer".to_owned(),
        "array" => "an array".to_owned(),
        "object" => "an object".to_owned(),
        other => format!("a {other}"),
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;

    /// An edit call names its step by the ops it made, once each and in order, and any
    /// other tool names nothing.
    #[test]
    fn an_edit_step_is_named_by_its_ops() {
        let args = json!({"ops": [
            {"op": "split", "at": "110f"},
            {"op": "trim_in", "at": "5f"},
            {"op": "split", "at": "115f"},
        ]});
        assert_eq!(edit_step_label("edit", &args).as_deref(), Some("Split, trim in"));
        assert_eq!(edit_step_label("undo", &args), None, "only an edit call names its step");
        assert_eq!(edit_step_label("edit", &json!({"ops": []})), None);
        let many = json!({"ops": [
            {"op": "remove_silence"}, {"op": "add_marker"}, {"op": "change_speed"},
        ]});
        let label = edit_step_label("edit", &many).expect("a label");
        assert!(label.chars().count() < 30, "{label:?} is too long for a menu row");
        assert!(label.starts_with("Remove silence"), "{label:?}");
    }

    fn request(server: &mut Server, method: &str, params: Value) -> Value {
        handle(server, json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})).expect("a reply")
    }

    fn call(server: &mut Server, name: &str, args: Value) -> Value {
        request(server, "tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn text_of(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap_or("")
    }

    /// A project path inside a folder that the returned guard removes when the test ends.
    fn scratch_project(test: &str) -> (TestDir, String) {
        let dir = TestDir::new(&format!("rpc-{test}"));
        let path = dir.join("rpc.edith").display().to_string();
        (dir, path)
    }

    #[test]
    fn initialize_advertises_resources() {
        let mut s = Server::new();
        let reply = request(&mut s, "initialize", json!({}));
        assert_eq!(reply["result"]["capabilities"]["resources"], json!({}));
        assert!(reply["result"]["instructions"].as_str().unwrap_or("").contains("V1@"));
    }

    #[test]
    fn resources_list_has_three() {
        let mut s = Server::new();
        let reply = request(&mut s, "resources/list", json!({}));
        let uris: Vec<&str> = reply["result"]["resources"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|r| r["uri"].as_str())
            .collect();
        assert_eq!(uris, ["edith://help", "edith://project", "edith://timeline"]);
    }

    #[test]
    fn resources_read_help_mentions_addresses_and_keep() {
        let mut s = Server::new();
        let reply = request(&mut s, "resources/read", json!({"uri": "edith://help"}));
        let content = &reply["result"]["contents"][0];
        assert_eq!(content["mimeType"], "text/markdown");
        let text = content["text"].as_str().unwrap_or("");
        assert!(text.contains("V1@"));
        assert!(text.contains("keep"));
    }

    #[test]
    fn resources_read_project_without_a_project_gives_the_error_text() {
        let mut s = Server::new();
        let reply = request(&mut s, "resources/read", json!({"uri": "edith://project"}));
        let text = reply["result"]["contents"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("no project open"), "{text}");
    }

    #[test]
    fn resources_read_refuses_an_unknown_uri() {
        let mut s = Server::new();
        let reply = request(&mut s, "resources/read", json!({"uri": "edith://nope"}));
        assert_eq!(reply["error"]["code"], -32002);
    }

    #[test]
    fn unknown_argument_is_refused_before_the_tool_runs() {
        let mut s = Server::new();
        let (_dir, path) = scratch_project("bogus");
        let result = call(&mut s, "project_new", json!({"path": path, "bogus": 1}));
        assert_eq!(result["isError"], true);
        let text = text_of(&result);
        assert!(text.starts_with("unknown argument `bogus` for project_new; accepted: "), "{text}");
        assert!(text.contains("path") && text.contains("discard"), "{text}");
        assert!(s.session.is_none(), "the tool must not have run");
    }

    #[test]
    fn missing_required_is_refused() {
        let mut s = Server::new();
        let result = call(&mut s, "project_new", json!({"fps": 30}));
        assert_eq!(result["isError"], true);
        assert_eq!(text_of(&result), "missing required argument `path` for project_new: Where the .edith file will be saved.");
    }

    #[test]
    fn wrong_type_is_refused() {
        let mut s = Server::new();
        let (_dir, path) = scratch_project("fps-string");
        let result = call(&mut s, "project_new", json!({"path": path, "fps": "30"}));
        assert_eq!(result["isError"], true);
        assert_eq!(text_of(&result), "argument `fps` for project_new must be a number, got string; for example 1: Frame rate of the timeline.");
    }

    #[test]
    fn a_list_type_accepts_either_listed_type() {
        let media = all_tools().into_iter().find(|t| t.name == "media_import").expect("media_import");
        assert!(check_args(&media, &json!({"paths": "x.mp4"})).is_ok());
        assert!(check_args(&media, &json!({"paths": ["a.mp4", "b.mp4"]})).is_ok());
    }

    #[test]
    fn a_list_type_refuses_a_value_matching_none_of_its_types() {
        let media = all_tools().into_iter().find(|t| t.name == "media_import").expect("media_import");
        let err = check_args(&media, &json!({"paths": 5})).expect_err("refused");
        assert_eq!(
            err,
            "argument `paths` for media_import must be an array or a string, got number; for example [\"clip.mp4\"]: Media file paths to import into the open project: a list, or one path as a string."
        );
    }

    #[test]
    fn a_refusal_names_the_example_from_the_schema() {
        let levels = all_tools().into_iter().find(|t| t.name == "levels").expect("levels");
        let err = check_args(&levels, &json!({"target": "A1", "per_sec": true})).expect_err("refused");
        assert_eq!(err, "argument `per_sec` for levels must be a number, got boolean; for example 10: Buckets per second: a number, how many values each second of sound gets (e.g. 10 for ten per second).");
    }

    /// A refusal carries the property's first sentence; an abbreviation such as `e.g.` does not end it.
    #[test]
    fn a_refusal_or_missing_argument_says_what_the_argument_means() {
        let levels = all_tools().into_iter().find(|t| t.name == "levels").expect("levels");
        let err = check_args(&levels, &json!({})).expect_err("target is required");
        assert_eq!(err, "missing required argument `target` for levels: A clip address (A1@0), a lane (A1), src:N for a whole source file, or \"mix\" for the timeline as exported.");
        assert_eq!(prop_sentence(Some(&json!({"description": "A list: one (e.g. 3). Then more."}))).as_deref(), Some("A list: one (e.g. 3)."));
        assert_eq!(prop_sentence(Some(&json!({"description": "No period here"}))).as_deref(), Some("No period here"));
        assert_eq!(prop_sentence(Some(&json!({}))), None);
        assert_eq!(prop_sentence(None), None);
    }

    #[test]
    fn valid_call_passes() {
        let mut s = Server::new();
        let (_dir, path) = scratch_project("fps-number");
        let result = call(&mut s, "project_new", json!({"path": path, "fps": 30}));
        assert_eq!(result["isError"], false, "{}", text_of(&result));
    }

    #[test]
    fn nested_edit_ops_are_left_to_the_ops() {
        let edit = all_tools().into_iter().find(|t| t.name == "edit").expect("edit is registered");
        let args = json!({"ops": [{"op": "place", "not_an_arg": 1}]});
        assert!(check_args(&edit, &args).is_ok());
    }

    #[test]
    fn unknown_tool_gets_an_edit_op_hint_or_near_names() {
        let mut s = Server::new();
        let op = call(&mut s, "lane_add", json!({}));
        assert_eq!(op["isError"], true);
        assert_eq!(
            text_of(&op),
            "lane_add is an edit op: edit {ops:[{op:\"lane_add\", …}]} — help {topic:\"lane_add\"}"
        );
        let near = call(&mut s, "project_sav", json!({}));
        assert!(text_of(&near).contains("project_save"), "{}", text_of(&near));
        let far = call(&mut s, "zzzzzzzzzz", json!({}));
        assert!(text_of(&far).starts_with("unknown tool `zzzzzzzzzz`"), "{}", text_of(&far));
    }

    #[test]
    fn a_batch_array_is_refused_whole_with_a_null_id() {
        let mut s = Server::new();
        for batch in [json!([]), json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}])] {
            let reply = handle(&mut s, batch).expect("a batch gets a reply");
            assert_eq!(reply["error"]["code"], -32600);
            assert_eq!(reply["error"]["message"], "batches are not supported");
            assert!(reply["id"].is_null());
        }
    }

    #[test]
    fn an_id_that_is_not_a_string_number_or_null_is_refused() {
        let mut s = Server::new();
        let reply = handle(&mut s, json!({"jsonrpc": "2.0", "id": {"a": 1}, "method": "ping"})).expect("a reply");
        assert_eq!(reply["error"]["code"], -32600);
        assert!(reply["id"].is_null());
        let ok = handle(&mut s, json!({"jsonrpc": "2.0", "id": "x", "method": "ping"})).expect("a reply");
        assert_eq!(ok["id"], "x");
        assert!(ok.get("result").is_some());
    }

    #[test]
    fn a_non_object_message_is_an_invalid_request() {
        let mut s = Server::new();
        let reply = handle(&mut s, json!(42)).expect("a reply");
        assert_eq!(reply["error"]["code"], -32600);
    }

    #[test]
    fn a_label_past_its_max_length_is_refused_before_the_tool_runs() {
        let mut s = Server::new();
        let long = "x".repeat(61);
        let result = call(&mut s, "edit", json!({"ops": [{"op": "split", "at": "1f"}], "label": long}));
        assert_eq!(result["isError"], true);
        assert_eq!(text_of(&result), "argument `label` for edit must be at most 60 characters, got 61");
        let ok = call(&mut s, "edit", json!({"ops": [{"op": "split", "at": "1f"}], "label": "x".repeat(60)}));
        assert!(!text_of(&ok).contains("must be at most"), "{}", text_of(&ok));
    }

    #[test]
    fn a_path_past_4096_bytes_is_refused_up_front() {
        let mut s = Server::new();
        let path = format!("/{}", "a".repeat(4096));
        let result = call(&mut s, "project_new", json!({"path": path}));
        assert_eq!(result["isError"], true);
        assert_eq!(
            text_of(&result),
            "argument `path` for project_new has a path of 4097 bytes; a path is at most 4096 bytes"
        );
        assert!(s.session.is_none(), "the tool must not have run");
    }

    /// A tool call that makes more engine edits than the 100-step cap is one undo step, and
    /// it is undoable whole: the ledger does not drop it the moment it lands.
    #[test]
    fn a_tool_call_past_the_cap_is_one_undo_step() {
        let mut s = Server::new();
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        s.session = Some(engine::PlaybackSession::open(&src).expect("open assets/test_av.mp4"));
        let ops: Vec<Value> = (1..=110).map(|f| json!({"op": "split", "at": format!("{f}f")})).collect();
        let edited = call(&mut s, "edit", json!({"ops": ops}));
        assert_eq!(edited["isError"], false, "{}", text_of(&edited));
        assert_eq!(s.ledger.len(), 1, "one call, one step");
        assert_eq!(s.session.as_ref().unwrap().undo_depth(), 1, "the engine holds one snapshot for it");
        assert!(!s.history_capped);

        let undone = call(&mut s, "undo", json!({}));
        assert_eq!(undone["isError"], false, "{}", text_of(&undone));
        assert!(text_of(&undone).starts_with("undid: split"), "{}", text_of(&undone));
        assert!(s.ledger.is_empty());
        assert_eq!(s.session.as_ref().unwrap().undo_depth(), 0);
    }

    #[test]
    fn help_tool_is_listed_and_schema_checked() {
        let mut s = Server::new();
        let listed = request(&mut s, "tools/list", json!({}));
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"help"));
        let result = call(&mut s, "help", json!({"topic": "nope"}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn integral_float_passes_the_integer_check() {
        let frame = crate::server::tools().into_iter().find(|t| t.name == "frame").expect("frame tool");
        assert!(check_args(&frame, &json!({"at": "0f", "width": 1000000000.0})).is_ok());
        assert!(check_args(&frame, &json!({"at": "0f", "width": 1000000000})).is_ok());
        assert!(check_args(&frame, &json!({"at": "0f", "width": 1e30})).is_ok());
    }

    #[test]
    fn fractional_number_still_fails_the_integer_check() {
        let frame = crate::server::tools().into_iter().find(|t| t.name == "frame").expect("frame tool");
        let err = check_args(&frame, &json!({"at": "0f", "width": 1.5})).expect_err("refused");
        assert_eq!(err, "argument `width` for frame must be an integer, got 1.5; for example 1: Width of the image in pixels; the height follows the aspect.");
    }
}
