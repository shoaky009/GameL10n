use std::{borrow::Cow, path::Path};

use serde_json::{Value, json};

use crate::{
    EngineError,
    adaptation::{Session, read_tool_result, readback_page, text, workspace_file},
    discovery::InventoryFile,
};

const INLINE_HISTORY_BYTES: usize = 2048;
const EXCERPT_BYTES: usize = 512;

fn excerpt(text: &str) -> &str {
    &text[..text.floor_char_boundary(text.len().min(EXCERPT_BYTES))]
}

pub(super) fn overview(state: &Session, files: &[InventoryFile]) -> Value {
    let verification = state.verification.as_ref().map(|result| {
        json!({
            "passed":result.passed,
            "originalRoundtripPassed":result.original_roundtrip_passed,
            "units":result.units,
            "runtimeTested":result.runtime_tested,
            "diagnostics":result.diagnostics.iter().take(6).map(|text| excerpt(text)).collect::<Vec<_>>(),
        })
    });
    let package = state.verified_package.as_ref().map(|package| {
        json!({"id":package["id"],"version":package["version"],"recognition":package["manifest"]["recognition"]})
    });
    let release_files = state
        .release_files
        .iter()
        .map(|file| {
            let path = format!("adaptation/current/{}", workspace_file(&file.path));
            json!({"evidencePath":path,"role":file.role,"purpose":excerpt(&file.purpose)})
        })
        .collect::<Vec<_>>();
    let result_references = state
        .decision
        .evidence_refs
        .iter()
        .rev()
        .filter(|reference| {
            matches!(
                reference.tool.as_str(),
                "test_adapter"
                    | "inspect_engine_reference"
                    | "compose_adapter_template"
                    | "copy_adapter_components"
            )
        })
        .take(8)
        .map(|reference| json!({"tool":reference.tool,"toolCallId":reference.tool_call_id}))
        .collect::<Vec<_>>();
    json!({
        "objective":state.objective,"summary":state.summary,"checkpoint":state.checkpoint,
        "verification":verification,"coverage":state.text_coverage,"family":state.family,
        "implementation":{
            "package":package,"releaseFiles":release_files,"engineReference":state.extra.get("engineReference"),
        },
        "resultReferences":result_references,"evidenceFiles":files,
    })
}

pub(super) fn request_messages(messages: &[Value], latest_exchange: usize) -> Cow<'_, [Value]> {
    let mut request = Cow::Borrowed(messages);
    for (index, message) in messages[..latest_exchange].iter().enumerate() {
        if message["role"] != "tool" {
            continue;
        }
        let Some(content) = message["content"]
            .as_str()
            .filter(|text| text.len() > INLINE_HISTORY_BYTES)
        else {
            continue;
        };
        let result: Value = serde_json::from_str(content).unwrap_or(Value::Null);
        let mut preview = json!({
            "truncated":true,"toolCallId":message["tool_call_id"],
            "preview":excerpt(result["content"].as_str().unwrap_or(content)),
        });
        for key in ["evidencePath", "offset", "nextOffset", "more"] {
            if let Some(value) = result.get(key) {
                preview[key] = value.clone();
            }
        }
        request.to_mut()[index]["content"] = json!(preview.to_string());
    }
    request
}

pub(super) const ADAPTATION_RESULT_PATH: &str = "adaptation/current/session.json";

pub(super) fn tool_result(
    workspace: &Path,
    state: &Session,
    messages: &[Value],
    args: &Value,
) -> Result<Value, EngineError> {
    let id = text(args, "toolCallId")?;
    match args["source"].as_str().unwrap_or("auto") {
        "auto" => {
            if let Some(page) = read_review_result(messages, id, args)? {
                return Ok(page);
            }
        }
        "review" => {
            return read_review_result(messages, id, args)?
                .ok_or_else(|| EngineError::Rejected("找不到本次总结的工具结果".into()));
        }
        "adaptation" => {}
        _ => return Err(EngineError::Protocol("无效的工具结果来源".into())),
    }
    let mut page = read_tool_result(workspace, state, args)?;
    page["evidencePath"] = json!(ADAPTATION_RESULT_PATH);
    Ok(page)
}

fn read_review_result(
    messages: &[Value],
    id: &str,
    args: &Value,
) -> Result<Option<Value>, EngineError> {
    let Some(content) = messages.iter().rev().find_map(|message| {
        (message["role"] == "tool" && message["tool_call_id"] == id)
            .then(|| message["content"].as_str())
            .flatten()
    }) else {
        return Ok(None);
    };
    let original: Value = serde_json::from_str(content)?;
    let mut page = readback_page(content, args, 0, true)?;
    page["toolCallId"] = json!(id);
    if let Some(path) = original.get("evidencePath") {
        page["evidencePath"] = path.clone();
    }
    Ok(Some(page))
}
