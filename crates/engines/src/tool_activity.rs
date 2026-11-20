use serde_json::Value;

/// User-visible tool activity includes targets, never source bodies or complete arguments.
pub(crate) fn message(name: &str, args: &Value) -> String {
    if !args.is_object() {
        return format!("正在执行 {name}");
    }
    let subject = match name {
        "list_files"
        | "read_file"
        | "search_text"
        | "inspect_pe_imports"
        | "inspect_pe_code"
        | "inspect_unity_schema" => {
            let root = match text(args, "root") {
                Some("game") => "游戏目录",
                Some("installed") => "已安装文件",
                _ => "工作区",
            };
            match text(args, "path") {
                Some(".") => root.into(),
                Some(path) => format!("{root} / {path}"),
                None if name == "list_files" => root.into(),
                None => String::new(),
            }
        }
        "write_file" | "replace_text" | "remove_file" => text(args, "path")
            .map(|path| format!("工作区 / {path}"))
            .unwrap_or_default(),
        "find_game_file" => text(args, "path")
            .map(|path| format!("游戏目录 / {path}"))
            .unwrap_or_default(),
        "download_tool" => fields(args, &["path", "url"]),
        "fetch_url" => fields(args, &["url"]),
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
    let options = match name {
        "list_files" => parameters(args, &["offset", "limit"]),
        "read_file" => parameters(args, &["offset", "length", "encoding"]),
        "search_text" => parameters(args, &["offset", "fileByteOffset", "encoding", "query"]),
        "fetch_url" => parameters(args, &["mode", "offset", "length", "query"]),
        "inspect_existing_text" => parameters(args, &["offset", "limit"]),
        "inspect_adapter_component" | "inspect_builtin_adapter" => {
            parameters(args, &["offset", "length"])
        }
        "inspect_adapter_source"
        | "inspect_engine_reference"
        | "read_evidence"
        | "read_tool_result"
        | "read_session_history" => parameters(args, &["offset", "length", "query"]),
        "write_file" => parameters(args, &["copyFrom"]),
        "run_engine_python" | "run_tool" => join(&[
            parameters(args, &["workingDirectory"]),
            command_arguments(args),
        ]),
        "test_adapter" => parameters(args, &["discoveryOnly"]),
        _ => String::new(),
    };
    let details = join(&[short(&subject, 152), options]);
    if details.is_empty() {
        format!("正在执行 {name}")
    } else {
        format!("正在执行 {name} · {}", short(&details, 256))
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

fn short(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut result = chars
        .by_ref()
        .take(limit)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn parameters(args: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| {
            let value = match args.get(*key)? {
                Value::String(value) if !value.trim().is_empty() => short(value, 56),
                Value::Number(value) => value.to_string(),
                Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            Some(format!("{key}={value}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_arguments(args: &Value) -> String {
    let Some(values) = args["arguments"]
        .as_array()
        .filter(|values| !values.is_empty())
    else {
        return String::new();
    };
    let flags = values
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|value| value.strip_prefix("--"))
        .map(|value| value.split('=').next().unwrap_or_default())
        .filter(|flag| {
            !flag.is_empty()
                && flag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .take(3)
        .map(|flag| format!("--{}", short(flag, 24)))
        .collect::<Vec<_>>();
    let remaining = values.len() - flags.len();
    let count = if remaining > 0 {
        format!("参数 {remaining} 项")
    } else {
        String::new()
    };
    join(&[flags.join(" "), count])
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
