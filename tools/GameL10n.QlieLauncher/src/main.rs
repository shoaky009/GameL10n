#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod config;
mod font;
mod self_test;
#[cfg(windows)]
mod windows;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    let result = run();
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            #[cfg(windows)]
            if std::env::args_os()
                .nth(1)
                .is_none_or(|arg| arg != "--self-test")
            {
                windows::show_error(&error.to_string());
            }
            eprintln!("{error}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--self-test" {
        self_test::run()?;
        return Ok(0);
    }
    #[cfg(windows)]
    {
        windows::launch(&args)
    }
    #[cfg(not(windows))]
    Err("QLIE 启动器仅支持 Windows。".into())
}
