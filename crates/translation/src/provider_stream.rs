//! Bounded SSE decoding and complete-turn assembly for adaptation requests.
use super::*;
use std::time::Instant;

const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
const MAX_IGNORED_EVENT_TYPES: usize = 16;
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

fn invalid(detail: &str) -> ProviderError {
    let mut error = ProviderError::new(
        diagnostic_codes::STREAM_FORMAT,
        "API 流式响应格式无效，未接受本次结果。",
    );
    error.diagnostic.detail = Some(detail.into());
    error.outcome_unknown = true;
    error
}

fn is_valid_event_type(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 128
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn sanitize_event_type(kind: &str) -> String {
    kind.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
    name: String,
    data: Vec<u8>,
    has_data: bool,
    started: bool,
    skip_lf: bool,
    searched: usize,
}

struct Event {
    name: String,
    data: Vec<u8>,
}

impl Decoder {
    fn push(
        &mut self,
        bytes: &[u8],
        mut accept: impl FnMut(Event) -> Result<bool, ProviderError>,
    ) -> Result<(), ProviderError> {
        self.pending.extend_from_slice(bytes);
        if !self.started {
            if self.pending.len() < UTF8_BOM.len() && UTF8_BOM.starts_with(&self.pending) {
                return Ok(());
            }
            self.started = true;
            if self.pending.starts_with(UTF8_BOM) {
                self.pending.drain(..UTF8_BOM.len());
            }
        }
        if self.skip_lf && !self.pending.is_empty() {
            if self.pending[0] == b'\n' {
                self.pending.remove(0);
            }
            self.skip_lf = false;
        }
        let mut offset = 0;
        let mut search_from = self.searched;
        while let Some(length) = self.pending[search_from..]
            .iter()
            .position(|b| matches!(b, b'\r' | b'\n'))
        {
            let end = search_from + length;
            self.skip_lf = self.pending[end] == b'\r' && end + 1 == self.pending.len();
            let next = end
                + 1
                + usize::from(
                    self.pending[end] == b'\r' && self.pending.get(end + 1) == Some(&b'\n'),
                );
            let line = &self.pending[offset..end];
            if line.len() > MAX_RESPONSE_BYTES {
                return Err(invalid("SSE line exceeds the response size limit."));
            }
            if line.is_empty() {
                if self.has_data {
                    let event = Event {
                        name: std::mem::take(&mut self.name),
                        data: std::mem::take(&mut self.data),
                    };
                    self.has_data = false;
                    if !accept(event)? {
                        self.pending.clear();
                        self.searched = 0;
                        return Ok(());
                    }
                } else {
                    self.name.clear();
                }
            } else if !line.starts_with(b":") {
                let split = line.iter().position(|b| *b == b':').unwrap_or(line.len());
                let field = &line[..split];
                let mut value = line.get(split + 1..).unwrap_or_default();
                if value.starts_with(b" ") {
                    value = &value[1..];
                }
                match field {
                    b"data" => {
                        if self.has_data {
                            self.data.push(b'\n');
                        }
                        self.data.extend_from_slice(value);
                        self.has_data = true;
                    }
                    b"event" => {
                        self.name = std::str::from_utf8(value)
                            .map_err(|_| invalid("SSE event name is not UTF-8."))?
                            .into()
                    }
                    _ => {}
                }
            }
            if self.data.len() > MAX_RESPONSE_BYTES {
                return Err(invalid("SSE event exceeds the response size limit."));
            }
            offset = next;
            search_from = next;
        }
        self.pending.drain(..offset);
        self.searched = self.pending.len();
        if self.pending.len() > MAX_RESPONSE_BYTES {
            return Err(invalid("SSE line exceeds the response size limit."));
        }
        Ok(())
    }
}

struct Chat {
    root: Value,
    calls: BTreeMap<u64, Value>,
    assembled_bytes: usize,
    finished: bool,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            root: json!({"choices":[{"index":0,"message":{"role":null,"content":null},"finish_reason":null}]}),
            calls: BTreeMap::new(),
            assembled_bytes: 0,
            finished: false,
        }
    }
}

fn identity(target: &mut Value, value: &Value) -> Result<(), ProviderError> {
    if value.is_null() {
        return Ok(());
    }
    if target.is_null() {
        *target = value.clone();
    } else if target != value {
        return Err(invalid("A stream changed an item or response identity."));
    }
    Ok(())
}

fn append(target: &mut Value, value: &Value, bytes: &mut usize) -> Result<(), ProviderError> {
    match value {
        Value::String(text) => {
            if target.is_null() {
                *target = json!("");
            }
            target
                .as_str()
                .ok_or_else(|| invalid("A streamed text field changed type."))?;
            *bytes = bytes.saturating_add(text.len());
            if *bytes > MAX_RESPONSE_BYTES {
                return Err(ProviderError::new(
                    diagnostic_codes::RESPONSE_SIZE_LIMIT,
                    "组装后的 API 响应超过大小限制。",
                ));
            }
            if let Value::String(output) = target {
                output.push_str(text);
            }
        }
        Value::Object(fields) => {
            if target.is_null() {
                *target = json!({});
            }
            if !target.is_object() {
                return Err(invalid("A streamed field changed type."));
            }
            for (key, value) in fields {
                append(&mut target[key], value, bytes)?;
            }
        }
        Value::Null => {}
        _ => {
            if target.is_null() {
                *bytes = bytes.saturating_add(value.to_string().len());
                *target = value.clone();
            } else if target != value {
                return Err(invalid("Unsupported changes to a streamed field."));
            }
        }
    }
    Ok(())
}

impl Chat {
    fn event(&mut self, value: Value) -> Result<(), ProviderError> {
        if let Some(id) = value.get("id") {
            identity(&mut self.root["id"], id)?;
        }
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            self.root["usage"] = usage.clone();
        }
        let choices = value["choices"]
            .as_array()
            .ok_or_else(|| invalid("Chat stream has no choices array."))?;
        if choices.is_empty() {
            return Ok(());
        }
        if choices.len() != 1 || choices[0]["index"] != 0 {
            return Err(ProviderError::new(
                diagnostic_codes::CHAT_CHOICES,
                "API 必须返回一个完整响应。",
            ));
        }
        if self.finished {
            return Err(invalid("Chat content arrived after the finish reason."));
        }
        let choice = &choices[0];
        let delta = choice["delta"]
            .as_object()
            .ok_or_else(|| invalid("Chat stream delta must be an object."))?;
        for (key, value) in delta {
            if key == "tool_calls" {
                if value.is_null() {
                    continue;
                }
                for call in value
                    .as_array()
                    .ok_or_else(|| invalid("Streamed tool calls must be an array."))?
                {
                    let index = call["index"]
                        .as_u64()
                        .filter(|i| *i < MAX_TOOL_CALLS as u64)
                        .ok_or_else(|| invalid("Invalid streamed tool-call index."))?;
                    let output = self.calls.entry(index).or_insert(json!({}));
                    let fields = call
                        .as_object()
                        .ok_or_else(|| invalid("Invalid streamed tool call."))?;
                    for (field, value) in fields {
                        if field == "index" {
                            continue;
                        }
                        if matches!(field.as_str(), "id" | "type") {
                            identity(&mut output[field], value)?;
                        } else {
                            append(&mut output[field], value, &mut self.assembled_bytes)?;
                        }
                    }
                }
            } else if key == "role" {
                identity(&mut self.root["choices"][0]["message"][key], value)?;
            } else {
                append(
                    &mut self.root["choices"][0]["message"][key],
                    value,
                    &mut self.assembled_bytes,
                )?;
            }
        }
        if let Some(finish) = choice.get("finish_reason").filter(|v| !v.is_null()) {
            if !finish.is_string() {
                return Err(invalid("Invalid finish reason."));
            }
            self.root["choices"][0]["finish_reason"] = finish.clone();
            self.finished = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ProviderError> {
        if !self.finished {
            return Err(ProviderError::new(
                diagnostic_codes::CHAT_INCOMPLETE,
                "模型未完成响应。",
            ));
        }
        if !self.calls.is_empty() {
            if self.calls.keys().copied().ne(0..self.calls.len() as u64) {
                return Err(invalid("Streamed tool-call indexes contain a gap."));
            }
            self.root["choices"][0]["message"]["tool_calls"] = json!(
                std::mem::take(&mut self.calls)
                    .into_values()
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }
}

struct Assembly {
    protocol: ApiProtocol,
    chat: Chat,
    response: Value,
    response_id: Value,
    items: BTreeMap<u64, (Value, Value)>,
    ignored_events: BTreeSet<String>,
    done: bool,
}

impl Assembly {
    fn new(protocol: ApiProtocol) -> Self {
        Self {
            protocol,
            chat: Chat::default(),
            response: Value::Null,
            response_id: Value::Null,
            items: BTreeMap::new(),
            ignored_events: BTreeSet::new(),
            done: false,
        }
    }

    fn root(&self) -> &Value {
        match self.protocol {
            ApiProtocol::ChatCompletions => &self.chat.root,
            ApiProtocol::Responses => &self.response,
        }
    }

    fn event(&mut self, event: Event) -> Result<(), ProviderError> {
        if event.data == b"[DONE]" {
            if self.protocol == ApiProtocol::ChatCompletions && !self.done {
                self.chat.finish()?;
                self.done = true;
                return Ok(());
            }
            return Err(invalid("Unexpected stream terminator."));
        }
        if self.done {
            return Err(invalid("Data arrived after the terminal event."));
        }
        let value: Value = serde_json::from_slice(&event.data)
            .map_err(|error| invalid(&format!("Invalid SSE JSON: {error}")))?;
        if !value.is_object() {
            return Err(invalid("SSE data must be an object."));
        }
        if event.name == "error"
            || value["type"] == "error"
            || value.get("error").is_some_and(|error| !error.is_null())
        {
            let mut error = ProviderError::new(
                diagnostic_codes::STREAM_ERROR,
                "服务商返回流式错误，未接受本次结果。",
            );
            error.outcome_unknown = true;
            return Err(error);
        }
        if self.protocol == ApiProtocol::ChatCompletions {
            return self.chat.event(value);
        }
        let kind = value["type"]
            .as_str()
            .ok_or_else(|| invalid("Responses event has no type."))?;
        if !event.name.is_empty() && event.name != kind {
            return Err(invalid(&format!(
                "SSE event name ({}) and payload type ({}) disagree.",
                sanitize_event_type(&event.name),
                sanitize_event_type(kind),
            )));
        }
        match kind {
            "response.completed"
            | "response.incomplete"
            | "response.failed"
            | "response.cancelled" => {
                let status = kind.strip_prefix("response.").unwrap_or_default();
                let response = &value["response"];
                self.response = response.clone();
                if !response.is_object() || response["status"] != status {
                    return Err(invalid(
                        "Responses terminal event and response status disagree.",
                    ));
                }
                if let Some(id) = response.get("id") {
                    identity(&mut self.response_id, id)?;
                }
                for (index, (id, kind)) in &self.items {
                    let item = &response["output"][*index as usize];
                    if &item["id"] != id || &item["type"] != kind {
                        return Err(invalid(
                            "Responses terminal output changed an item identity.",
                        ));
                    }
                }
                self.done = true;
            }
            "response.created" | "response.in_progress" | "response.queued" => {
                if !value["response"].is_object() {
                    return Err(invalid("Responses lifecycle event has no response."));
                }
                if let Some(id) = value["response"].get("id") {
                    identity(&mut self.response_id, id)?;
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                let index = value["output_index"]
                    .as_u64()
                    .filter(|i| *i < MAX_TOOL_CALLS as u64)
                    .ok_or_else(|| invalid("Invalid Responses output index."))?;
                let item = &value["item"];
                if !item.is_object() || !item["id"].is_string() || !item["type"].is_string() {
                    return Err(invalid("Invalid Responses output item event."));
                }
                let saved = self
                    .items
                    .entry(index)
                    .or_insert((Value::Null, Value::Null));
                identity(&mut saved.0, &item["id"])?;
                identity(&mut saved.1, &item["type"])?;
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta" => self.output_event(&value, "delta")?,
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_text.done" => self.output_event(
                &value,
                if kind == "response.refusal.done" {
                    "refusal"
                } else {
                    "text"
                },
            )?,
            "response.function_call_arguments.done" => self.output_event(&value, "arguments")?,
            "response.content_part.added"
            | "response.content_part.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => {
                if !value["part"].is_object() {
                    return Err(invalid("Invalid Responses content part."));
                }
                self.output_identity(&value)?;
            }
            "response.output_text.annotation.added" => {
                if !value["annotation"].is_object() {
                    return Err(invalid("Invalid Responses annotation."));
                }
                self.output_identity(&value)?;
            }
            _ => {
                if !is_valid_event_type(kind) {
                    return Err(invalid(&format!(
                        "Unsupported Responses stream event: {}.",
                        sanitize_event_type(kind),
                    )));
                }
                if let Some(id) = value.get("response").and_then(|response| response.get("id")) {
                    identity(&mut self.response_id, id)?;
                }
                if let Some(index) = value.get("output_index").and_then(Value::as_u64)
                    && value.get("item_id").is_some()
                    && self.items.contains_key(&index)
                {
                    self.output_identity(&value)?;
                }
                if self.ignored_events.len() < MAX_IGNORED_EVENT_TYPES {
                    self.ignored_events.insert(kind.to_owned());
                }
            }
        }
        Ok(())
    }

    fn output_identity(&self, value: &Value) -> Result<(), ProviderError> {
        let index = value["output_index"]
            .as_u64()
            .ok_or_else(|| invalid("Missing Responses output index."))?;
        let saved = self
            .items
            .get(&index)
            .ok_or_else(|| invalid("Responses delta has no output item."))?;
        if value["item_id"] != saved.0 {
            return Err(invalid(
                "Responses delta refers to a different output item.",
            ));
        }
        Ok(())
    }

    fn output_event(&self, value: &Value, field: &str) -> Result<(), ProviderError> {
        self.output_identity(value)?;
        if !value[field].is_string() {
            return Err(invalid(
                "Responses delta or completed text is not a string.",
            ));
        }
        Ok(())
    }
}

impl OpenAiProvider {
    pub(super) async fn receive_agent_stream(
        &self,
        request: reqwest::RequestBuilder,
        payload_len: usize,
        names: Option<&HashSet<String>>,
        cancellation: &CancellationToken,
        attempted: &mut bool,
    ) -> Result<Completion, ProviderError> {
        let started = Instant::now();
        let first_deadline = tokio::time::Instant::now() + self.analysis_first_response_timeout;
        let mut timing = AnalysisStreamTiming::default();
        let mut first = None;
        let mut last = None;
        let mut maximum_idle = Duration::ZERO;
        let mut assembly = Assembly::new(self.settings.protocol);
        let mut usage_recorded = false;
        let outcome = async {
            let response = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return Err(ProviderError::new(diagnostic_codes::CANCELLED, "任务已取消。"));
                }
                result = tokio::time::timeout_at(first_deadline, async {
                    self.request_count.fetch_add(1, Ordering::Relaxed);
                    *attempted = true;
                    request.send().await
                }) => match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        if error.is_timeout() {
                            timing.timeout_stage = Some(AnalysisStreamTiming::TIMEOUT_STAGE_FIRST_RESPONSE);
                        }
                        return Err(ProviderError::transport(error, diagnostic_codes::NETWORK_ERROR, "发送分析请求或等待 HTTP 响应头失败", &self.settings.api_key));
                    }
                    Err(_) => {
                        timing.timeout_stage = Some(AnalysisStreamTiming::TIMEOUT_STAGE_FIRST_RESPONSE);
                        let mut error = ProviderError::new(diagnostic_codes::REQUEST_TIMEOUT, "等待分析响应头超时。");
                        error.diagnostic.detail = Some(format!("等待 HTTP 响应头达到 {} ms", self.analysis_first_response_timeout.as_millis()));
                        return Err(error);
                    }
                }
            };
            let status = response.status();
            if !status.is_success() {
                let mut error = ProviderError::new(
                    diagnostic_codes::HTTP_ERROR,
                    &format!("API 请求失败（HTTP {}）。", status.as_u16()),
                );
                error.status_code = Some(status.as_u16());
                error.outcome_unknown = status.is_server_error();
                error.retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(retry_after);
                return Err(error);
            }
            let event_stream = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
            if !event_stream {
                return Err(invalid("Analysis response is not text/event-stream."));
            }
            let mut response = response;
            let mut decoder = Decoder::default();
            let mut wire_bytes = 0_usize;
            let mut deadline = first_deadline;
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Err(ProviderError::new(diagnostic_codes::CANCELLED, "任务已取消。"));
                    }
                    result = tokio::time::timeout_at(deadline,response.chunk()) => match result {
                        Ok(Ok(chunk)) => chunk,
                        Ok(Err(cause)) => {
                            if cause.is_timeout() {
                                timing.timeout_stage = Some(if first.is_none() {
                                    AnalysisStreamTiming::TIMEOUT_STAGE_FIRST_RESPONSE
                                } else {
                                    AnalysisStreamTiming::TIMEOUT_STAGE_STREAM_IDLE
                                });
                            }
                            return Err(ProviderError::transport(cause, diagnostic_codes::NETWORK_INTERRUPTED,
                                &format!("读取 SSE 响应体失败（已收 {wire_bytes} 字节）"), &self.settings.api_key));
                        }
                        Err(_) => {
                            timing.timeout_stage = Some(if first.is_none() {
                                AnalysisStreamTiming::TIMEOUT_STAGE_FIRST_RESPONSE
                            } else {
                                AnalysisStreamTiming::TIMEOUT_STAGE_STREAM_IDLE
                            });
                            let mut error = ProviderError::new(diagnostic_codes::REQUEST_TIMEOUT, "分析响应等待超时。");
                            error.diagnostic.detail = Some(if first.is_none() {
                                format!("HTTP 已返回，等待 SSE 首字节达到请求时限 {} ms", self.analysis_first_response_timeout.as_millis())
                            } else {
                                format!("已收 {wire_bytes} 字节，连续 {} ms 未收到 SSE 数据", self.analysis_stream_idle_timeout.as_millis())
                            });
                            return Err(error);
                        }
                    }
                };
                let Some(chunk) = chunk else {
                    let mut error = ProviderError::new(diagnostic_codes::STREAM_INCOMPLETE, "API 流式响应缺少终止事件。");
                    error.diagnostic.detail = Some(format!("SSE 响应体已到 EOF，共 {wire_bytes} 字节，缺少终止事件"));
                    return Err(error);
                };
                if chunk.is_empty() {
                    continue;
                }
                let now = Instant::now();
                if let Some(previous) = last {
                    maximum_idle = maximum_idle.max(now.duration_since(previous));
                }
                first.get_or_insert(now);
                last = Some(now);
                deadline = tokio::time::Instant::now() + self.analysis_stream_idle_timeout;
                wire_bytes = wire_bytes.saturating_add(chunk.len());
                if wire_bytes > MAX_STREAM_BYTES {
                    return Err(ProviderError::new(
                        diagnostic_codes::RESPONSE_SIZE_LIMIT,
                        "API 流式传输超过大小限制。",
                    ));
                }
                decoder.push(&chunk, |event| {
                    assembly.event(event)?;
                    Ok(!assembly.done)
                })?;
                if assembly.done {
                    let root = assembly.root();
                    let output_len = serde_json::to_vec(root)
                        .map_err(|_| invalid("Cannot encode the completed response."))?
                        .len();
                    if output_len > MAX_RESPONSE_BYTES {
                        return Err(ProviderError::new(
                            diagnostic_codes::RESPONSE_SIZE_LIMIT,
                            "组装后的 API 响应超过大小限制。",
                        ));
                    }
                    usage_recorded = true;
                    return self.completion_from_root(root, payload_len, output_len, names);
                }
            }
        }
        .await;
        let ended = Instant::now();
        if let Some(first) = first {
            timing.first_response_ms = Some(milliseconds(first.duration_since(started)));
            timing.stream_duration_ms = Some(milliseconds(ended.duration_since(first)));
            if let Some(last) = last {
                maximum_idle = maximum_idle.max(ended.duration_since(last));
            }
            timing.maximum_idle_ms = Some(milliseconds(maximum_idle));
        }
        if !assembly.ignored_events.is_empty() {
            timing.ignored_events = std::mem::take(&mut assembly.ignored_events)
                .into_iter()
                .collect();
        }
        match outcome {
            Ok(mut completion) => {
                completion.stream_timing = Some(timing);
                Ok(completion)
            }
            Err(mut error) => {
                if !assembly.done && !error.is_cancelled() && error.status_code.is_none() {
                    error.outcome_unknown = true;
                }
                if !usage_recorded && !assembly.root()["usage"].is_null() {
                    let usage = read_usage(assembly.root(), self.settings.protocol, payload_len, 0);
                    self.record_usage(&usage);
                    error.usage = usage;
                }
                if error.diagnostic.request_id.is_none() {
                    error.diagnostic.request_id = assembly.root()["id"]
                        .as_str()
                        .or_else(|| assembly.response_id.as_str())
                        .filter(|id| {
                            id.len() <= MAX_REQUEST_ID_LEN
                                && !id.chars().any(char::is_control)
                                && (self.settings.api_key.is_empty()
                                    || !id.contains(&self.settings.api_key))
                        })
                        .map(str::to_owned);
                }
                error.stream_timing = Some(Box::new(timing));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::tests::{
        agent_messages, agent_tools, local_agent, read_mock_request, stream_fixture,
    };
    use std::io::Write;

    fn data(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    fn chat_done() -> String {
        stream_fixture(
            json!({"id":"chat-stream","choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"完成"}}],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":8}}}),
            ApiProtocol::ChatCompletions,
        )
    }

    fn response_done() -> Value {
        json!({"id":"resp-stream","status":"completed","output":[
            {"id":"reason-1","type":"reasoning","encrypted_content":"opaque-secret","summary":[],"extension":{"preserved":true}},
            {"id":"item-1","type":"function_call","call_id":"call-1","name":"read","arguments":"{\"path\":\"资源\"}","status":"completed","extension":"preserved"}
        ],"usage":{"input_tokens":12,"output_tokens":3,"input_tokens_details":{"cached_tokens":8}}})
    }

    fn decode(
        assembly: &mut Assembly,
        decoder: &mut Decoder,
        bytes: &[u8],
    ) -> Result<(), ProviderError> {
        if assembly.done {
            return Ok(());
        }
        decoder.push(bytes, |event| {
            assembly.event(event)?;
            Ok(!assembly.done)
        })
    }

    #[test]
    fn sse_accepts_bom_utf8_multiline_data_and_all_line_endings_at_every_split() {
        let wire = "\u{feff}: heartbeat\r\nevent: message\rdata: {\"text\":\r\ndata: \"你好\"}\r\r"
            .as_bytes();
        for split in 0..=wire.len() {
            let mut decoder = Decoder::default();
            let mut events = Vec::new();
            for bytes in [&wire[..split], &wire[split..]] {
                decoder
                    .push(bytes, |event| {
                        events.push(event);
                        Ok(true)
                    })
                    .unwrap();
            }
            assert_eq!(events.len(), 1, "split {split}");
            assert_eq!(events[0].name, "message");
            assert_eq!(
                serde_json::from_slice::<Value>(&events[0].data).unwrap(),
                json!({"text":"你好"})
            );
        }
    }

    #[test]
    fn chat_assembles_indexed_calls_null_fields_reasoning_and_final_usage_before_done() {
        let wire = [
            data(json!({"id":"chat-stream","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning_content":"推"},"finish_reason":null}]})),
            data(json!({"id":null,"choices":[{"index":0,"delta":{"role":null,"tool_calls":[{"index":1,"id":"call-2","type":"function","function":{"name":"re","arguments":"{"}},{"index":0,"id":"call-1","type":"function","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":null}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":null,"type":null,"function":{"name":null,"arguments":"\"资源\"}"}},{"index":1,"function":{"name":"ad","arguments":"}"}}],"reasoning_content":"理"},"finish_reason":null}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":null,"role":null},"finish_reason":"tool_calls"}]})),
            data(json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":8}}})),
        ].concat();
        let mut assembly = Assembly::new(ApiProtocol::ChatCompletions);
        let mut decoder = Decoder::default();
        for byte in wire.as_bytes() {
            decode(&mut assembly, &mut decoder, &[*byte]).unwrap();
        }
        assert!(!assembly.done);
        decode(&mut assembly, &mut decoder, b"data: [DONE]\n\n").unwrap();
        let usage = read_usage(assembly.root(), ApiProtocol::ChatCompletions, 0, 0);
        let completion =
            read_agent_chat(assembly.root(), &usage, &HashSet::from(["read".into()])).unwrap();
        assert_eq!(completion.tool_calls[0].arguments, "{\"path\":\"资源\"}");
        assert_eq!(completion.tool_calls[1].name, "read");
        assert_eq!(
            completion.provider_data.as_ref().unwrap()["reasoning_content"],
            "推理"
        );
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
            (12, 3, Some(8))
        );
    }

    #[test]
    fn responses_keep_full_terminal_output_and_reject_identity_changes() {
        let final_response = response_done();
        let events = [
            json!({"type":"response.created","response":{"id":"resp-stream","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"reason-1","type":"reasoning"}}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"id":"item-1","type":"function_call"}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"item-1","delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.done","output_index":1,"item_id":"item-1","arguments":"{\"path\":\"资源\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"id":"item-1","type":"function_call"}}),
            json!({"type":"response.completed","response":final_response}),
        ];
        let wire = events.into_iter().map(data).collect::<String>();
        let mut assembly = Assembly::new(ApiProtocol::Responses);
        let mut decoder = Decoder::default();
        for byte in wire.as_bytes() {
            decode(&mut assembly, &mut decoder, &[*byte]).unwrap();
        }
        let usage = read_usage(assembly.root(), ApiProtocol::Responses, 0, 0);
        let completion =
            read_agent_responses(assembly.root(), &usage, &HashSet::from(["read".into()])).unwrap();
        assert_eq!(
            completion.provider_data,
            Some(final_response["output"].clone())
        );
        assert_eq!(
            completion.output_items,
            final_response["output"].as_array().unwrap().clone()
        );
        assert_eq!((usage.input_tokens, usage.cached_tokens), (12, Some(8)));

        for wrong in [
            json!({"type":"response.completed","response":{"id":"other","status":"completed","output":[]}}),
            json!({"type":"response.completed","response":{"id":"resp-stream","status":"failed"}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"wrong","delta":"x"}),
        ] {
            let mut assembly = Assembly::new(ApiProtocol::Responses);
            let mut decoder = Decoder::default();
            decode(
                &mut assembly,
                &mut decoder,
                data(json!({"type":"response.created","response":{"id":"resp-stream"}})).as_bytes(),
            )
            .unwrap();
            decode(&mut assembly,&mut decoder,data(json!({"type":"response.output_item.added","output_index":1,"item":{"id":"item-1","type":"function_call"}})).as_bytes()).unwrap();
            assert_eq!(
                decode(&mut assembly, &mut decoder, data(wrong).as_bytes())
                    .unwrap_err()
                    .diagnostic
                    .code,
                "stream-format"
            );
        }
    }

    #[test]
    fn terminal_events_ignore_trailing_data_independently_of_fragmentation() {
        for (protocol, terminal) in [
            (ApiProtocol::ChatCompletions, chat_done()),
            (
                ApiProtocol::Responses,
                stream_fixture(response_done(), ApiProtocol::Responses),
            ),
        ] {
            let wire = format!("{terminal}data: [DONE]\n\n: partial heartbeat\r\nevent: \u{fffd}");
            for split in 0..=wire.len() {
                let mut assembly = Assembly::new(protocol);
                let mut decoder = Decoder::default();
                decode(&mut assembly, &mut decoder, &wire.as_bytes()[..split]).unwrap();
                decode(&mut assembly, &mut decoder, &wire.as_bytes()[split..]).unwrap();
                assert!(assembly.done, "split {split}");
            }
        }
    }

    fn server(
        parts: Vec<(Duration, Vec<u8>)>,
        finish: bool,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let (body_tx, body_rx) = tokio::sync::oneshot::channel();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.set_nodelay(true).unwrap();
            let request = read_mock_request(&mut stream);
            assert_eq!(request["stream"], true);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            let _ = headers_tx.send(());
            let mut body_tx = Some(body_tx);
            for (delay, bytes) in parts {
                std::thread::sleep(delay);
                if bytes.is_empty() {
                    continue;
                }
                if write!(stream, "{:x}\r\n", bytes.len())
                    .and_then(|_| stream.write_all(&bytes))
                    .and_then(|_| stream.write_all(b"\r\n"))
                    .is_err()
                {
                    return;
                }
                if let Some(tx) = body_tx.take() {
                    let _ = tx.send(());
                }
            }
            if finish {
                let _ = stream.write_all(b"0\r\n\r\n");
            }
        });
        (format!("http://{address}/v1"), headers_rx, body_rx, handle)
    }

    async fn complete(
        provider: &OpenAiProvider,
        cancellation: &CancellationToken,
    ) -> Result<Completion, ProviderError> {
        provider
            .complete_agent(&agent_messages(), &agent_tools(), &[], 128, cancellation)
            .await
    }

    #[tokio::test]
    async fn streams_outlive_first_response_and_normal_request_deadlines_while_active() {
        for protocol in [ApiProtocol::ChatCompletions, ApiProtocol::Responses] {
            let terminal = if protocol == ApiProtocol::Responses {
                stream_fixture(response_done(), protocol)
            } else {
                chat_done()
            };
            let mut parts = vec![(Duration::ZERO, b": heartbeat\n\n".to_vec())];
            parts
                .extend((0..14).map(|_| (Duration::from_millis(100), b": heartbeat\n\n".to_vec())));
            parts.push((Duration::from_millis(100), terminal.into_bytes()));
            let (endpoint, _, _, handle) = server(parts, true);
            let mut provider = local_agent(endpoint, protocol);
            provider.request_timeout = Duration::from_millis(50);
            provider.analysis_first_response_timeout = Duration::from_millis(300);
            provider.analysis_stream_idle_timeout = Duration::from_millis(500);
            let completion = complete(&provider, &CancellationToken::new())
                .await
                .unwrap();
            let timing = completion.stream_timing.unwrap();
            assert!(timing.stream_duration_ms.unwrap() >= 1400);
            assert!(timing.maximum_idle_ms.unwrap() >= 50);
            assert_eq!(timing.timeout_stage, None);
            assert_eq!(provider.request_count(), 1);
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn first_body_and_idle_timeouts_report_distinct_stages_without_retry() {
        for idle in [false, true] {
            let mut parts = Vec::new();
            if idle {
                parts.push((Duration::ZERO, b": heartbeat\n\n".to_vec()));
            }
            parts.push((Duration::from_millis(500), chat_done().into_bytes()));
            let (endpoint, _, _, handle) = server(parts, true);
            let mut provider = local_agent(endpoint, ApiProtocol::ChatCompletions);
            provider.analysis_first_response_timeout = if idle {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(200)
            };
            provider.analysis_stream_idle_timeout = Duration::from_millis(200);
            let error = complete(&provider, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(error.diagnostic.code, "request-timeout");
            assert!(error.outcome_unknown);
            let timing = error.stream_timing.unwrap();
            assert_eq!(
                timing.timeout_stage,
                Some(if idle {
                    "stream-idle"
                } else {
                    "first-response"
                })
            );
            assert_eq!(timing.first_response_ms.is_some(), idle);
            assert_eq!(provider.request_count(), 1);
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn review_retries_the_same_turn_after_a_response_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stalled, _) = listener.accept().unwrap();
            let original = read_mock_request(&mut stalled);
            let (mut resumed, _) = listener.accept().unwrap();
            let retry = read_mock_request(&mut resumed);
            let body = stream_fixture(response_done(), ApiProtocol::Responses);
            write!(resumed, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            (original, retry)
        });
        let mut provider = local_agent(endpoint, ApiProtocol::Responses);
        provider.analysis_first_response_timeout = Duration::from_millis(500);
        let retries = std::sync::Mutex::new(Vec::new());
        let completion = provider
            .complete_review(
                &agent_messages(),
                &agent_tools(),
                128,
                &CancellationToken::new(),
                &|error, retry| {
                    retries
                        .lock()
                        .unwrap()
                        .push((retry, error.diagnostic.code.clone()));
                },
            )
            .await
            .unwrap();
        assert_eq!(completion.tool_calls[0].name, "read");
        assert_eq!(
            retries.into_inner().unwrap(),
            [(1, "request-timeout".into())]
        );
        let (original, retry) = server.join().unwrap();
        assert_eq!(original, retry);
        let metrics = provider.request_metrics();
        assert_eq!((metrics.requests, metrics.failed_requests), (2, 1));
    }

    #[tokio::test]
    async fn cancellation_interrupts_first_body_and_active_stream_waits() {
        for active in [false, true] {
            let mut parts = Vec::new();
            if active {
                parts.push((Duration::ZERO, b": heartbeat\n\n".to_vec()));
            }
            parts.push((Duration::from_millis(300), chat_done().into_bytes()));
            let (endpoint, headers, body, handle) = server(parts, true);
            let provider = local_agent(endpoint, ApiProtocol::ChatCompletions);
            let cancellation = CancellationToken::new();
            let cancel = cancellation.clone();
            let cancel_when_ready = async move {
                if active {
                    body.await.unwrap();
                } else {
                    headers.await.unwrap();
                }
                cancel.cancel();
            };
            let (result, ()) = tokio::join!(complete(&provider, &cancellation), cancel_when_ready);
            assert!(result.unwrap_err().is_cancelled());
            assert_eq!(provider.request_count(), 1);
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn incomplete_malformed_and_error_streams_never_return_partial_calls() {
        let cases = [
            (ApiProtocol::ChatCompletions,data(json!({"id":"partial","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":3}})),"stream-incomplete"),
            (ApiProtocol::ChatCompletions,"data: {broken}\n\n".into(),"stream-format"),
            (ApiProtocol::ChatCompletions,data(json!({"choices":[{"index":0,"delta":"wrong"}]})),"stream-format"),
            (ApiProtocol::ChatCompletions,"event: error\ndata: {\"error\":{\"message\":\"failed\"}}\n\n".into(),"stream-error"),
            (ApiProtocol::Responses,data(json!({"type":"response.created","response":{"id":"partial"}})),"stream-incomplete"),
            (ApiProtocol::Responses,"event: response.completed\ndata: {\"type\":\"response.in_progress\",\"response\":{}}\n\n".into(),"stream-format"),
            (ApiProtocol::Responses,data(json!({"type":"error","message":"failed"})),"stream-error"),
        ];
        for (protocol, wire, code) in cases {
            let (endpoint, _, _, handle) = server(vec![(Duration::ZERO, wire.into_bytes())], true);
            let provider = local_agent(endpoint, protocol);
            let error = complete(&provider, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(error.diagnostic.code, code);
            assert!(error.outcome_unknown);
            if protocol == ApiProtocol::ChatCompletions && code == "stream-incomplete" {
                assert_eq!(error.diagnostic.request_id.as_deref(), Some("partial"));
                assert_eq!(
                    (error.usage.input_tokens, error.usage.output_tokens),
                    (12, 3)
                );
            }
            assert_eq!(provider.request_count(), 1);
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn interrupted_http_body_is_a_network_failure() {
        let (endpoint, _, _, handle) =
            server(vec![(Duration::ZERO, b": heartbeat\n\n".to_vec())], false);
        let mut provider = local_agent(endpoint, ApiProtocol::ChatCompletions);
        provider.client = reqwest::Client::builder().no_proxy().build().unwrap();
        provider.analysis_stream_idle_timeout = Duration::from_secs(10);
        let error = complete(&provider, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.diagnostic.code, "network-interrupted");
        assert!(error.outcome_unknown);
        handle.join().unwrap();
    }

    #[test]
    fn assembled_content_and_sse_events_are_bounded() {
        let mut assembly = Assembly::new(ApiProtocol::ChatCompletions);
        let mut decoder = Decoder::default();
        let fragment = "x".repeat(MAX_RESPONSE_BYTES / 2 + 1);
        let wire = data(
            json!({"choices":[{"index":0,"delta":{"content":fragment},"finish_reason":null}]}),
        );
        decode(&mut assembly, &mut decoder, wire.as_bytes()).unwrap();
        assert_eq!(
            decode(&mut assembly, &mut decoder, wire.as_bytes())
                .unwrap_err()
                .diagnostic
                .code,
            "response-size-limit"
        );
        let mut decoder = Decoder::default();
        assert_eq!(
            decoder
                .push(&vec![b'x'; MAX_RESPONSE_BYTES + 1], |_| Ok(true))
                .unwrap_err()
                .diagnostic
                .code,
            "stream-format"
        );
    }

    #[test]
    fn chat_rejects_non_null_identity_changes_and_index_gaps() {
        for delta in [
            json!({"role":"user"}),
            json!({"tool_calls":[{"index":0,"id":"other"}]}),
            json!({"tool_calls":[{"index":0,"type":"other"}]}),
        ] {
            let mut assembly = Assembly::new(ApiProtocol::ChatCompletions);
            let mut decoder = Decoder::default();
            decode(&mut assembly,&mut decoder,data(json!({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function"}]}}]})).as_bytes()).unwrap();
            assert_eq!(
                decode(
                    &mut assembly,
                    &mut decoder,
                    data(json!({"choices":[{"index":0,"delta":delta}]})).as_bytes()
                )
                .unwrap_err()
                .diagnostic
                .code,
                "stream-format"
            );
        }
        let mut assembly = Assembly::new(ApiProtocol::ChatCompletions);
        let mut decoder = Decoder::default();
        let wire = data(
            json!({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":1,"id":"call-1","type":"function"}]},"finish_reason":"tool_calls"}]}),
        ) + "data: [DONE]\n\n";
        assert_eq!(
            decode(&mut assembly, &mut decoder, wire.as_bytes())
                .unwrap_err()
                .diagnostic
                .code,
            "stream-format"
        );
    }

    #[tokio::test]
    async fn output_limits_keep_final_usage_and_response_ids_for_both_protocols() {
        for protocol in [ApiProtocol::ChatCompletions, ApiProtocol::Responses] {
            let body = match protocol {
                ApiProtocol::ChatCompletions => {
                    json!({"id":"limited","choices":[{"finish_reason":"length","message":{"role":"assistant","content":"partial"}}],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":8}}})
                }
                ApiProtocol::Responses => {
                    json!({"id":"limited","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[],"usage":{"input_tokens":12,"output_tokens":3,"input_tokens_details":{"cached_tokens":8}}})
                }
            };
            let (endpoint, _, _, handle) = server(
                vec![(Duration::ZERO, stream_fixture(body, protocol).into_bytes())],
                true,
            );
            let provider = local_agent(endpoint, protocol);
            let error = complete(&provider, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(error.diagnostic.code, "output-token-limit");
            assert!(!error.outcome_unknown);
            assert_eq!(error.diagnostic.request_id.as_deref(), Some("limited"));
            assert_eq!(
                (
                    error.usage.input_tokens,
                    error.usage.output_tokens,
                    error.usage.cached_tokens
                ),
                (12, 3, Some(8))
            );
            assert_eq!(provider.request_metrics().usage_requests, 1);
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn heartbeat_wire_limit_is_independent_of_assembled_content() {
        let heartbeat = format!(":{}\n", "h".repeat(1024 * 1024 - 2)).into_bytes();
        let parts = (0..65)
            .map(|_| (Duration::ZERO, heartbeat.clone()))
            .collect();
        let (endpoint, _, _, handle) = server(parts, true);
        let mut provider = local_agent(endpoint, ApiProtocol::ChatCompletions);
        provider.client = reqwest::Client::builder().no_proxy().build().unwrap();
        provider.analysis_stream_idle_timeout = Duration::from_secs(10);
        let error = complete(&provider, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.diagnostic.code, "response-size-limit");
        assert!(error.message.contains("流式传输"));
        assert!(error.outcome_unknown);
        handle.join().unwrap();
    }

    #[test]
    fn responses_ignore_well_formed_extension_events_and_report_invalid_event_names() {
        let final_response = response_done();
        let events = [
            json!({"type":"response.created","response":{"id":"resp-stream","status":"in_progress"}}),
            json!({"type":"keepalive"}),
            json!({"type":"codex.rate_limits","remaining":42}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"reason-1","type":"reasoning"}}),
            json!({"type":"response.reasoning_summary.delta","output_index":0,"item_id":"reason-1","delta":"thinking"}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"id":"item-1","type":"function_call"}}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"id":"item-1","type":"function_call"}}),
            json!({"type":"response.completed","response":final_response}),
        ];
        let wire = events.into_iter().map(data).collect::<String>();
        let mut assembly = Assembly::new(ApiProtocol::Responses);
        let mut decoder = Decoder::default();
        decode(&mut assembly, &mut decoder, wire.as_bytes()).unwrap();
        assert!(assembly.done);
        assert_eq!(
            assembly.ignored_events,
            BTreeSet::from([
                "codex.rate_limits".to_owned(),
                "keepalive".to_owned(),
                "response.reasoning_summary.delta".to_owned(),
            ])
        );

        let mut bad = Assembly::new(ApiProtocol::Responses);
        let mut decoder = Decoder::default();
        let err = decode(
            &mut bad,
            &mut decoder,
            data(json!({"type":"bad event!"})).as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.diagnostic.code, "stream-format");
        assert!(
            err.diagnostic
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("bad event!")),
            "{:?}",
            err.diagnostic.detail
        );

        let mut mismatch = Assembly::new(ApiProtocol::Responses);
        let mut decoder = Decoder::default();
        decode(
            &mut mismatch,
            &mut decoder,
            data(json!({"type":"response.created","response":{"id":"resp-stream"}})).as_bytes(),
        )
        .unwrap();
        let err = decode(
            &mut mismatch,
            &mut decoder,
            data(json!({"type":"response.custom_ping","response":{"id":"other-resp"}})).as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.diagnostic.code, "stream-format");

        let mut cancelled = Assembly::new(ApiProtocol::Responses);
        let mut decoder = Decoder::default();
        decode(
            &mut cancelled,
            &mut decoder,
            data(json!({"type":"response.cancelled","response":{"id":"resp-stream","status":"cancelled"}})).as_bytes(),
        )
        .unwrap();
        assert!(cancelled.done);
    }
}
