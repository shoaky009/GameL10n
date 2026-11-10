//! Request-only history previews. Persisted evidence and provider reasoning remain unchanged.
use super::*;

pub(super) struct Replay {
    pub messages: Vec<Value>,
    pub omitted: usize,
    pub process_bytes_saved: usize,
}

pub(super) fn calls(message: &Value) -> Vec<(String, String)> {
    fn collect(value: &Value, result: &mut Vec<(String, String)>) {
        if let Some(items) = value.as_array() {
            for item in items {
                collect(item, result);
            }
        } else if value["type"] == "function_call" {
            if let (Some(id), Some(name)) = (value["call_id"].as_str(), value["name"].as_str()) {
                result.push((id.into(), name.into()));
            }
        } else {
            for field in ["tool_calls", "toolCalls"] {
                if let Some(items) = value[field].as_array() {
                    for item in items {
                        let name = item["function"]["name"]
                            .as_str()
                            .or_else(|| item["name"].as_str());
                        if let (Some(id), Some(name)) = (item["id"].as_str(), name) {
                            result.push((id.into(), name.into()));
                        }
                    }
                }
            }
            if let Some(provider) = value.get("providerData") {
                collect(provider, result);
            }
        }
    }
    let mut result = Vec::new();
    collect(message, &mut result);
    result
}

pub(super) fn successful(value: &Value) -> bool {
    if value["success"] != true {
        return false;
    }
    for result in [value, &value["result"]] {
        if result["exitCode"].as_i64().is_some_and(|code| code != 0)
            || result["passed"] == false
            || result["compiled"] == false
            || result.get("error").is_some_and(|error| !error.is_null())
            || matches!(
                result["status"].as_str(),
                Some("failed" | "rejected" | "no-text" | "limit-exceeded")
            )
        {
            return false;
        }
    }
    true
}

fn head(value: &str, length: usize) -> String {
    value.chars().take(length).collect()
}
fn tail(value: &str, length: usize) -> String {
    value
        .chars()
        .rev()
        .take(length)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn process_preview(value: &Value, original: &str, id: &str) -> String {
    let mut value = value.clone();
    let mut streams = Vec::new();
    for nested in [false, true] {
        let target = if nested {
            &mut value["result"]
        } else {
            &mut value
        };
        if !target.is_object() {
            continue;
        }
        let reported = target["report"].is_object();
        for (name, limit, start) in if reported {
            [("stdout", 512, 256), ("stderr", 2048, 1024)]
        } else {
            [("stdout", 4000, 2000), ("stderr", 3000, 1000)]
        } {
            let Some(text) = target[name].as_str() else {
                continue;
            };
            let count = text.chars().count();
            if count <= limit {
                continue;
            }
            let shortened = format!(
                "{}\n[... {} characters omitted ...]\n{}",
                head(text, start),
                count - limit,
                tail(text, limit - start)
            );
            streams.push(json!({"path":format!("{}{name}",if nested{"result."}else{""}),"originalCharacters":count,"omittedCharacters":count-limit}));
            target[name] = json!(shortened);
        }
    }
    if streams.is_empty() {
        return original.into();
    }
    value["processOutputPreview"] = json!({"truncated":true,"toolCallId":id,"originalCharacters":original.chars().count(),"streams":streams,"readBack":"read_tool_result(toolCallId, offset, length) reads the retained original JSON, including omitted output; query can locate a diagnostic without rerunning the process."});
    value.to_string()
}

pub(super) fn for_request(history: &[Value]) -> Replay {
    let retrieved: BTreeSet<_> = history
        .iter()
        .flat_map(calls)
        .filter(|(_, name)| name == "read_tool_result")
        .map(|(id, _)| id)
        .collect();
    let mut result = Replay {
        messages: history.to_vec(),
        omitted: 0,
        process_bytes_saved: 0,
    };
    let mut seen = BTreeMap::new();
    for message in &mut result.messages {
        if message["role"] != "tool" {
            continue;
        }
        let Some(id) = message["tool_call_id"]
            .as_str()
            .or_else(|| message["toolCallId"].as_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(original) = message["content"].as_str() else {
            continue;
        };
        if retrieved.contains(&id) {
            continue;
        }
        let parsed = serde_json::from_str(original).unwrap_or(Value::Null);
        let success = successful(&parsed);
        let preview = process_preview(&parsed, original, &id);
        result.process_bytes_saved += original.len().saturating_sub(preview.len());
        let fingerprint = (original.len() > 2048).then(|| hash_text(original));
        let previous = fingerprint.as_ref().and_then(|key| seen.get(key));
        let reported = parsed["report"].is_object() || parsed["result"]["report"].is_object();
        if (success && preview.len() > 8192 && !reported) || previous.is_some() {
            message["content"] = json!(json!({"success":success,"previousResultOmitted":true,"toolCallId":id,"originalCharacters":original.chars().count(),"identicalTo":previous,"preview":head(&preview,1536),"tail":tail(&preview,512),"readBack":if previous.is_some(){"Exact repeat of the earlier identicalTo result; use that result or read_tool_result to retrieve the retained original without rerunning the operation."}else{"read_tool_result(toolCallId, offset, length, query) reads the retained original result. Omitted is not missing or failed."}}).to_string());
            result.omitted += 1;
        } else if preview != original {
            message["content"] = json!(preview);
            result.omitted += 1;
        }
        if let Some(key) = fingerprint {
            seen.entry(key).or_insert(id);
        }
    }
    result
}

pub(super) fn flatten(messages: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    for message in messages {
        if let Some(provider) = message.get("providerData").filter(|value| !value.is_null()) {
            if let Some(items) = provider.as_array() {
                result.extend(items.iter().cloned());
            } else {
                result.push(provider.clone());
            }
        } else {
            result.push(message.clone());
        }
    }
    result
}

// Old Rust sessions stored Responses items separately. Treat contiguous reasoning/calls
// and their results as one exchange, just like the C# providerData envelope.
fn exchanges(history: &[Value]) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = None;
    let mut has_result = false;
    for (i, message) in history.iter().enumerate() {
        let user = message["role"] == "user" || message["role"] == "system";
        let tool = message["role"] == "tool" || message["type"] == "function_call_output";
        let assistant =
            message["role"] == "assistant" || (!user && !tool && message.get("type").is_some());
        if user || (assistant && has_result) {
            if let Some(begin) = start.take() {
                groups.push(begin..i);
            }
            has_result = false;
        }
        if assistant && start.is_none() {
            start = Some(i);
        }
        has_result |= tool;
    }
    if let Some(begin) = start {
        groups.push(begin..history.len());
    }
    groups
}

pub(super) fn recent_call_names(history: &[Value]) -> BTreeSet<String> {
    exchanges(history)
        .iter()
        .rev()
        .take(4)
        .flat_map(|group| {
            history[group.clone()]
                .iter()
                .flat_map(calls)
                .map(|(_, name)| name)
        })
        .collect()
}

/// Select whole exchanges before previewing or cloning their potentially large source writes.
pub(super) fn decision_window(history: &[Value], problem_start: usize) -> std::ops::Range<usize> {
    let mut start = history.len();
    let mut bytes = 0;
    for group in exchanges(history).iter().rev().take(4) {
        if start != history.len() && group.end <= problem_start {
            break;
        }
        let size = history[group.clone()]
            .iter()
            .map(visible_json_bytes)
            .sum::<usize>();
        if start != history.len() && bytes + size > 64 * 1024 {
            break;
        }
        start = group.start;
        bytes += size;
    }
    if start == history.len() {
        // The initial inventory and trailing host notices are ordinary user messages.
        start = 0;
    }
    start..history.len()
}

pub(super) fn recent(history: &[Value], skip_latest: bool) -> Vec<Value> {
    let replay = for_request(history);
    let groups = exchanges(history);
    let count = groups.len().saturating_sub(usize::from(skip_latest));
    let Some(last) = groups.get(count.saturating_sub(1)).filter(|_| count > 0) else {
        return vec![];
    };
    let end = last.end;
    let mut start = end;
    for group in groups[..count].iter().rev().take(4) {
        if group.end != start
            || (start != end
                && replay.messages[group.start..end]
                    .iter()
                    .map(visible_json_bytes)
                    .sum::<usize>()
                    > 64 * 1024)
        {
            break;
        }
        start = group.start;
    }
    history[start..end].to_vec()
}

pub(super) fn failures(history: &[Value], start: usize, base: usize) -> Vec<Value> {
    let calls: BTreeMap<_, _> = history
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            calls(message)
                .into_iter()
                .map(move |(id, name)| (id, (name, base + index)))
        })
        .collect();
    history
        .iter()
        .enumerate()
        .skip(start)
        .rev()
        .filter_map(|(index, message)| {
            let id = message["tool_call_id"]
                .as_str()
                .or_else(|| message["call_id"].as_str())?;
            let content = message["content"]
                .as_str()
                .or_else(|| message["output"].as_str())?;
            let mut result: Value = serde_json::from_str(content).ok()?;
            result.as_object_mut()?.entry("success").or_insert(json!(true));
            if successful(&result) {
                return None;
            }
            let (tool, call_index) = calls.get(id)?;
            // Keep observed diagnostics and readback locations, without interpreting a
            // failed command as proof that the entire implementation route is invalid.
            let preview = process_preview(&result, content, id);
            let diagnostics = [&result, &result["result"]]
                .into_iter()
                .flat_map(|value| {
                    ["error", "diagnostics", "stage", "phase", "exitCode", "timedOut", "nextAction"]
                        .into_iter()
                        .filter_map(move |key| value.get(key).map(|value| format!("{key}: {}", head(&value.to_string(), 2048))))
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(json!({"toolCallId":id,"tool":tool,"callMessageIndex":call_index,"messageIndex":base+index,"diagnostics":head(&diagnostics,2048),"preview":head(&preview,1024),"tail":tail(&preview,512)}))
        })
        .take(8)
        .collect()
}

pub(super) fn archive_index(history: &[Value], start: usize, base: usize) -> Value {
    json!(history.iter().enumerate().skip(start).filter(|(_,m)|m["role"]!="user").rev().take(16).map(|(index,message)|json!({"messageIndex":base+index,"role":message["role"],"toolCallId":message.get("tool_call_id").or_else(||message.get("call_id")),"preview":head(&visible_json(message).to_string(),512)})).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_preserve_failure_metadata_readback_and_originals() {
        let failure=json!({"success":false,"phase":"build","timedOut":true,"stdout":"a".repeat(20000),"stderr":"b".repeat(5000)}).to_string();
        let original = vec![
            json!({"type":"function_call","call_id":"r","name":"read_tool_result","arguments":"{}"}),
            json!({"role":"tool","tool_call_id":"p","content":failure}),
            json!({"role":"tool","tool_call_id":"r","content":json!({"success":true,"content":"c".repeat(20000)}).to_string()}),
        ];
        let copy = original.clone();
        let replay = for_request(&original);
        assert!(replay.process_bytes_saved > 15000);
        let preview: Value =
            serde_json::from_str(replay.messages[1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(preview["phase"], "build");
        assert_eq!(preview["timedOut"], true);
        assert_eq!(replay.messages[2], original[2]);
        assert_eq!(original, copy);
    }

    #[test]
    fn report_previews_keep_the_structured_findings_and_original_streams_retrievable() {
        let result = json!({"success":true,"stdout":"x".repeat(20000),"stderr":"y".repeat(8000),
            "report":{"status":"completed","tested":24,"matched":2,"summary":"说明".repeat(600),"failures":vec!["失败".repeat(160);4]}});
        let messages =
            [json!({"role":"tool","tool_call_id":"experiment","content":result.to_string()})];
        let preview = for_request(&messages);
        let value: Value =
            serde_json::from_str(preview.messages[0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(value["report"], result["report"]);
        assert!(value["stdout"].as_str().unwrap().len() < 1024);
        assert!(
            value["processOutputPreview"]["readBack"]
                .as_str()
                .unwrap()
                .contains("read_tool_result")
        );
        assert_eq!(messages[0]["content"], result.to_string());
    }
    #[test]
    fn recent_exchanges_do_not_split_responses_batches_or_reasoning() {
        let history = vec![
            json!({"role":"user","content":"task"}),
            json!({"type":"reasoning","encrypted_content":"unchanged"}),
            json!({"type":"function_call","call_id":"a","name":"read_file"}),
            json!({"type":"function_call","call_id":"b","name":"read_file"}),
            json!({"role":"tool","tool_call_id":"a","content":"a"}),
            json!({"role":"tool","tool_call_id":"b","content":"b"}),
            json!({"role":"assistant","tool_calls":[{"id":"c","function":{"name":"checkpoint"}}]}),
            json!({"role":"tool","tool_call_id":"c","content":"ok"}),
        ];
        assert_eq!(recent(&history, true), history[1..6]);
        assert_eq!(recent(&history, false), history[1..]);
    }

    #[test]
    fn oversized_latest_exchange_keeps_every_call_result_and_provider_reasoning() {
        let batch = vec![
            json!({"role":"assistant","providerData":[{"type":"reasoning","encrypted_content":"retained"},{"type":"function_call","call_id":"a","name":"replace_text","arguments":"large active source change"},{"type":"function_call","call_id":"b","name":"run_engine_python","arguments":"{}"}]}),
            json!({"role":"tool","tool_call_id":"a","content":json!({"success":false,"error":"source mismatch","diagnostics":"x".repeat(70000)}).to_string()}),
            json!({"role":"tool","tool_call_id":"b","content":json!({"success":false,"error":"preceding edit failed"}).to_string()}),
        ];
        let mut history = vec![
            json!({"role":"assistant","tool_calls":[{"id":"old","function":{"name":"read_file"}}]}),
            json!({"role":"tool","tool_call_id":"old","content":"old"}),
        ];
        history.extend(batch.clone());
        assert_eq!(recent(&history, false), batch);
        let failures = failures(&history, 0, 10);
        assert_eq!(failures[0]["toolCallId"], "b");
        assert_eq!(failures[0]["callMessageIndex"], 12);
        assert_eq!(failures[0]["messageIndex"], 14);
        assert!(
            failures[1]["preview"]
                .as_str()
                .unwrap()
                .contains("source mismatch")
        );
    }
}
