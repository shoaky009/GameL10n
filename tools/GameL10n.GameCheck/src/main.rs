//! Short-lived game checks; image pixels remain in memory.

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod native;
#[path = "../../../crates/core/src/game_check/protocol.rs"]
mod protocol;

use protocol::{CheckEvent, CheckRequest, CheckResult, CheckStatus, Observation};
use std::io::{Read, Write};

fn emit(event: CheckEvent) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &event).map_err(|e| e.to_string())?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(65537)
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    if input.len() > 65536 {
        return Err("运行检查请求过长。".into());
    }
    #[cfg(windows)]
    if std::env::args().any(|arg| arg == "--cleanup") {
        let windows: Vec<protocol::ErrorWindow> =
            serde_json::from_slice(&input).map_err(|_| "错误窗口记录无效。")?;
        if windows.len() > 32 {
            return Err("错误窗口数量超出清理范围。".into());
        }
        native::close_error_windows(&windows);
        return Ok(());
    }
    let request: CheckRequest = serde_json::from_slice(&input).map_err(|_| "运行检查请求无效。")?;
    request.settings.validate()?;
    if request.expected.len() > 8
        || request.expected.iter().any(|t| t.len() > 12000)
        || request.arguments.len() > 30000
        || request
            .arguments
            .chars()
            .any(|c| c.is_control() && c != '\t')
    {
        return Err("运行检查参数过长或包含无效字符。".into());
    }
    #[cfg(windows)]
    {
        native::run(&request)
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        Err("运行检查仅支持 Windows。".into())
    }
}

fn main() {
    if let Err(message) = run() {
        let result = CheckResult {
            launch: Observation::new(CheckStatus::Inconclusive, &message),
            text: Observation::new(CheckStatus::Inconclusive, message),
            ..Default::default()
        };
        let _ = emit(CheckEvent::Launch(result.launch));
        let _ = emit(CheckEvent::Text(result.text));
    }
}
