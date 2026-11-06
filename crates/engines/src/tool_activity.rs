use serde_json::Value;

/// User-visible tool activity includes targets, never source bodies or complete arguments.
pub(crate) fn message(name: &str, args: &Value) -> String {
    if !args.is_object() {
        return format!("正在执行 {name}。");
    }
    let subject = match name {
        "list_files"
        | "read_file"
        | "search_text"
        | "inspect_pe_imports"
        | "inspect_pe_code"
        | "inspect_unity_schema" => {
            let root = if text(args, "root") == Some("game") {
                "游戏目录"
            } else {
                "工作区"
            };
            text(args, "path")
                .or_else(|| (name == "list_files").then_some("."))
                .map(|path| format!("{root} / {path}"))
                .unwrap_or_default()
        }
        "write_file" | "replace_text" | "remove_file" => text(args, "path")
            .map(|path| format!("工作区 / {path}"))
            .unwrap_or_default(),
        "find_game_file" => text(args, "path")
            .map(|path| format!("游戏目录 / {path}"))
            .unwrap_or_default(),
        "download_tool" => fields(args, &["path", "url"]),
        "fetch_url" => fields(args, &["url", "query"]),
        "inspect_existing_text" => fields(args, &["resourcePath"]),
        "read_evidence" => fields(args, &["path"]),
        "read_tool_result" => fields(args, &["toolCallId"]),
        "read_session_history" => args["messageIndex"]
            .as_u64()
            .map(|index| format!("会话消息 {index}"))
            .unwrap_or_else(|| "适配会话".into()),
        "inspect_engine_knowledge" | "read_knowledge" => text(args, "id")
            .or_else(|| text(args, "query"))
            .unwrap_or("")
            .into(),
        "list_adapters" | "search_engine_references" | "search_repositories" => {
            fields(args, &["query"])
        }
        "inspect_builtin_adapter" => fields(args, &["format", "path"]),
        "inspect_adapter_source" => fields(args, &["id", "version", "path"]),
        "inspect_adapter_component" => fields(args, &["id", "path"]),
        "inspect_engine_reference" => fields(args, &["engine", "path"]),
        "copy_adapter" => join(&[fields(args, &["id", "version"]), items(args, "files")]),
        "copy_engine_knowledge_tools" => fields(args, &["id", "tool"]),
        "inspect_engine_component" | "compose_adapter_template" => fields(args, &["id"]),
        "inspect_siglus_game" => fields(args, &["source"]),
        "run_engine_python" => text(args, "script")
            .or_else(|| text(args, "module"))
            .unwrap_or("")
            .into(),
        "run_tool" => fields(args, &["executable"]),
        "run_engine_component" => {
            let mut subject = fields(args, &["id", "operation", "archive"]);
            if let Some(output) = text(args, "output") {
                subject.push_str(&format!(" → {output}"));
            }
            subject
        }
        "try_builtin_adapter" => {
            join(&[fields(args, &["format", "directory"]), items(args, "files")])
        }
        "stage_resources" => items(args, "files"),
        "copy_adapter_components" => items(args, "ids"),
        "load_tool_groups" => items(args, "groups"),
        "update_release_files" => {
            let removed = items(args, "remove");
            join(&[
                items(args, "files"),
                if removed.is_empty() {
                    removed
                } else {
                    format!("移除 {removed}")
                },
            ])
        }
        "test_adapter" => join(&[
            text(args, "name")
                .or_else(|| text(args, "key"))
                .unwrap_or("当前适配器")
                .into(),
            items(args, "trialFiles"),
        ]),
        _ => String::new(),
    };
    if subject.is_empty() {
        format!("正在执行 {name}。")
    } else {
        let subject = subject
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>();
        format!("正在执行 {name}：{subject}")
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
}

fn fields(args: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| text(args, key))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn join(values: &[String]) -> String {
    values
        .iter()
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn items(args: &Value, key: &str) -> String {
    let Some(values) = args[key].as_array() else {
        return String::new();
    };
    let mut result = values
        .iter()
        .take(3)
        .filter_map(|value| value.as_str().or_else(|| text(value, "path")))
        .collect::<Vec<_>>()
        .join("、");
    if values.len() > 3 {
        result.push_str(&format!(" 等 {} 项", values.len()));
    }
    result
}
