//! Temporary native game window with launcher handoff and early-exit modes.
#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
#[expect(
    clippy::zombie_processes,
    reason = "The fixture launcher exits while its child keeps running."
)]
fn main() -> windows::core::Result<()> {
    use windows::{
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::*,
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::*,
        },
        core::w,
    };
    let arguments: Vec<_> = std::env::args().collect();
    std::fs::write(
        std::env::current_exe().unwrap().with_extension("pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    if arguments.iter().any(|a| a == "--error-dialog") {
        unsafe {
            MessageBoxW(
                None,
                w!("The game cannot load its resources."),
                w!("Game fixture - Application Error"),
                MB_OK | MB_ICONERROR,
            );
        }
        return Ok(());
    }
    if let Some(exit) = arguments.iter().find_map(|a| a.strip_prefix("--exit=")) {
        std::process::exit(exit.parse().unwrap());
    }
    if arguments.iter().any(|a| a == "--no-window") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return Ok(());
    }
    if let Some(child) = arguments.iter().find_map(|a| a.strip_prefix("--handoff=")) {
        std::process::Command::new(child).spawn().unwrap();
        if !arguments.iter().any(|arg| arg == "--keep-window") {
            return Ok(());
        }
    }
    unsafe extern "system" fn procedure(
        window: HWND,
        message: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
        unsafe {
            match message {
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(window, &mut paint);
                    let font = CreateFontW(
                        -30,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        DEFAULT_QUALITY,
                        0,
                        w!("Microsoft YaHei"),
                    );
                    let previous = SelectObject(dc, font.into());
                    let launcher =
                        std::env::current_exe().unwrap().file_stem().unwrap() == "Launcher";
                    let text: Vec<_> = if launcher {
                        "Launcher still running"
                    } else {
                        "Welcome to the translated world"
                    }
                    .encode_utf16()
                    .collect();
                    let _ = TextOutW(dc, 20, 35, &text);
                    let chinese: Vec<_> = if launcher {
                        "启动器窗口"
                    } else {
                        "欢迎来到这里，勇敢的旅行者。"
                    }
                    .encode_utf16()
                    .collect();
                    let _ = TextOutW(dc, 20, 105, &chinese);
                    SelectObject(dc, previous);
                    let _ = DeleteObject(font.into());
                    let _ = EndPaint(window, &paint);
                    LRESULT(0)
                }
                WM_TIMER => {
                    if w.0 == 2 {
                        let _ = KillTimer(Some(window), 2);
                        MessageBoxW(
                            Some(window),
                            w!("A game resource failed to load."),
                            w!("Game fixture - Application Error"),
                            MB_OK | MB_ICONERROR,
                        );
                        return LRESULT(0);
                    }
                    let _ = DestroyWindow(window);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(window, message, w, l),
            }
        }
    }
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("GameL10nCheckFixture"),
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("GameL10n runtime fixture"),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            600,
            260,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(window);
        SetTimer(Some(window), 1, 20000, None);
        if arguments.iter().any(|a| a == "--late-error") {
            SetTimer(Some(window), 2, 4500, None);
        }
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}
#[cfg(not(windows))]
fn main() {}
