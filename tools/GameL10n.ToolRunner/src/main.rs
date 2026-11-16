#[cfg(windows)]
mod native;

fn main() {
    #[cfg(windows)]
    let result = native::run();
    #[cfg(not(windows))]
    let result: Result<u32, Box<dyn std::error::Error>> =
        Err("当前平台不支持 AppContainer 执行隔离。".into());
    match result {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            eprintln!("GameL10n-Isolation-Failed: {error}");
            std::process::exit(1);
        }
    }
}
