//! Bounded process-protocol fixture for host cancellation and result handling.
use std::io::{Read, Write};
fn main() {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    if std::env::args().any(|a| a == "--cleanup") {
        let executable = std::path::Path::new(request[0]["executable"].as_str().unwrap());
        std::fs::write(executable.with_file_name("check-cleaned"), "closed").unwrap();
        return;
    }
    let game = std::path::Path::new(request["gameDirectory"].as_str().unwrap());
    std::fs::write(game.join("check-started"), "ready").unwrap();
    match std::fs::read_to_string(game.join("check-behavior"))
        .unwrap_or_default()
        .as_str()
    {
        "wait" => std::thread::sleep(std::time::Duration::from_secs(60)),
        "malformed" => println!("invalid event"),
        "oversize" => {
            std::io::stdout().write_all(&vec![b'x'; 9000]).unwrap();
        }
        "error" => {
            println!(
                "{}",
                serde_json::json!({"kind":"errorWindow","value":{"window":1,"processId":1,"processStarted":1,"executable":game.join("Game.exe")}})
            );
            println!(
                "{}",
                serde_json::json!({"kind":"launch","value":{"status":"failed","message":"检测到游戏错误窗口。"}})
            );
        }
        _ => {
            if request["mode"] != "text" {
                println!(
                    "{}",
                    serde_json::json!({"kind":"launch","value":{"status":"passed","message":"游戏窗口已出现。"}})
                );
            }
            if request["mode"] != "launch" {
                println!(
                    "{}",
                    serde_json::json!({"kind":"text","value":{"status":"inconclusive","message":"画面文字尚不稳定。"}})
                );
            }
        }
    }
}
