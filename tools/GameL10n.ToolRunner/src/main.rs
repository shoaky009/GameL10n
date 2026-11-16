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
            eprintln!(
                "GameL10n-Isolation-Failed: {error}\n请修复隔离运行环境后重试：确认运行工具完整、Windows 支持 AppContainer，并且输入和工作目录允许设置访问权限；也可以在“程序设置”中关闭“隔离执行 LLM 脚本与生成适配器”。关闭后将增加恶意指令或提示词投毒导致文件泄露、修改或删除的风险。"
            );
            std::process::exit(1);
        }
    }
}
