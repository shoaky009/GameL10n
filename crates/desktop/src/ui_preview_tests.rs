use crate::view::{App, MainWindow, NamedRow, ProjectRow, TextRow, TokenDisplay};
use slint::{
    ComponentHandle, Model, ModelRc, VecModel,
    platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use std::{cell::Cell, path::Path, rc::Rc, time::Duration};

fn save_bitmap(path: &Path, width: u32, height: u32, pixels: &[slint::Rgb8Pixel]) {
    let stride = (width * 3).div_ceil(4) * 4;
    let size = 54 + stride * height;
    let mut bytes = vec![0; size as usize];
    bytes[..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&size.to_le_bytes());
    bytes[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&width.to_le_bytes());
    bytes[22..26].copy_from_slice(&height.to_le_bytes());
    bytes[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24_u16.to_le_bytes());
    for y in 0..height {
        for x in 0..width {
            let pixel = pixels[(y * width + x) as usize];
            let offset = (54 + (height - 1 - y) * stride + x * 3) as usize;
            bytes[offset..offset + 3].copy_from_slice(&[pixel.b, pixel.g, pixel.r]);
        }
    }
    std::fs::write(path, bytes).unwrap();
}

#[test]
#[ignore = "Renders isolated desktop fixtures to target/ui-preview."]
fn render_desktop_preview_fixtures() {
    struct Backend {
        window: Rc<MinimalSoftwareWindow>,
        elapsed: Rc<Cell<Duration>>,
    }
    impl Platform for Backend {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.window.clone())
        }

        fn duration_since_start(&self) -> Duration {
            self.elapsed.get()
        }
    }
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let elapsed = Rc::new(Cell::new(Duration::from_millis(1)));
    slint::platform::set_platform(Box::new(Backend {
        window: window.clone(),
        elapsed: elapsed.clone(),
    }))
    .unwrap();
    let ui = MainWindow::new().unwrap();
    let app = ui.global::<App>();
    app.on_concise_text(|text| crate::recognition_ui::concise_text(&text).into());
    app.on_task_api_for(|operation, text| {
        crate::recognition_ui::task_api(&operation, &text).into()
    });
    app.set_ready(true);
    app.set_project_id("fixture".into());
    app.set_project_name("星海旅人 · 完整汉化项目".into());
    app.set_project_path("D:\\Games\\Visual Novels\\星海旅人\\Game\\资源与启动目录".into());
    app.set_project_files_path("D:\\GameL10n Data\\games\\fixture".into());
    app.set_projects(ModelRc::new(VecModel::from(vec![
        ProjectRow {
            id: "fixture".into(),
            name: "星海旅人 · 完整汉化项目".into(),
            path: "D:\\Games\\星海旅人".into(),
            status: "已提取 12,840 条文本".into(),
            ..Default::default()
        },
        ProjectRow {
            id: "second".into(),
            name: "雨后书店".into(),
            path: "D:\\Games\\雨后书店".into(),
            status: "覆盖待确认".into(),
            ..Default::default()
        },
    ])));
    app.set_counts("12,840 条 · 已译 8,210 · 已校对 2,460 · 格式问题 3 · 翻译失败 2".into());
    app.set_recognition("已提取 12,840 条文本 · 186 个脚本 / 资源".into());
    app.set_recognition_ready(true);
    app.set_recognition_engine("KiriKiri".into());
    app.set_recognition_adapter("engine.kirikiri · 版本 1.2.0".into());
    app.set_recognition_warnings(
        "已覆盖脚本中的角色名、对白和选项；图片文字与系统菜单仍需确认。\n中文显示使用随补丁安装的字体，需在游戏内检查换行、存档和读档效果。".into(),
    );
    app.set_review_summary("已整理脚本文本提取、占位符保护及原文字节回填的验证经验。".into());
    app.set_review_summary_usage(crate::token_ui::analysis_usage(
        1_518_192, 33_415, 1_408_640, Some(60), false,
    ));
    app.set_status("文本可试译".into());
    app.set_resources(ModelRc::new(VecModel::from(vec![
        "全部资源".into(),
        "scenario/chapter01.ks".into(),
    ])));
    let rows = [
        (
            "旁白 · 对白 / 旁白",
            "空に浮かぶ島を、もう一度見たかった。",
            "我想再看一眼那座浮在天空中的岛。",
            1,
            false,
            false,
            false,
            false,
        ),
        (
            "店员 · 对白 / 旁白",
            "ここは私たちの小さな書店です。",
            "这里是我们的小书店。",
            2,
            true,
            false,
            false,
            false,
        ),
        (
            "旅人 · 对白 / 旁白",
            "明日の朝、駅で待っています。",
            "明早我在车站等你。",
            1,
            true,
            false,
            false,
            false,
        ),
        (
            "店员 · 对白 / 旁白",
            "[name]、忘れないで。",
            "别忘了。",
            0,
            false,
            true,
            true,
            true,
        ),
        (
            "选项",
            "旅を続けますか？",
            "",
            0,
            false,
            false,
            false,
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(i, (metadata, source, translation, origin, reviewed, invalid, failed, candidate))| {
            TextRow {
                id: format!("line-{i}").into(),
                location: format!("scenario/chapter01.ks · 第 {} 行", 128 + i).into(),
                source: source.into(),
                translation: translation.into(),
                metadata: metadata.into(),
                origin,
                reviewed,
                invalid,
                failed,
                candidate,
                selected: i == 1,
            }
        },
    )
    .collect::<Vec<_>>();
    app.set_texts(ModelRc::new(VecModel::from(rows.clone())));
    app.set_text_total(12840);
    app.set_shown(5);
    app.set_selected_texts(1);
    app.set_editor_id("line-1".into());
    app.set_editor_location("scenario/chapter01.ks\n位置：129 · dialogue".into());
    app.set_editor_source("ここは私たちの小さな書店です。".into());
    app.set_editor_text("这里是我们的小书店。".into());
    app.set_editor_origin(2);
    app.set_editor_reviewed(true);
    app.set_editor_status("已保存".into());
    app.set_editor_validation("文本格式检查通过。".into());
    app.set_editor_context("相邻文本：\n门口的风铃轻轻响起。\n角色：店员".into());
    app.set_patch_summary("已生成试译补丁 · 24 条译文 · 资源结构检查通过".into());
    app.set_patch_path("D:\\GameL10n Data\\games\\fixture\\patches\\current".into());
    app.set_launch_summary("Game.exe · 使用游戏目录作为工作目录".into());
    app.set_launch_can_run(true);
    app.set_log_text(
        "[信息] 已载入项目与保存的校对结果\n[信息] 自动译文与人工修订分别显示来源".into(),
    );
    ui.show().unwrap();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-preview");
    std::fs::create_dir_all(&directory).unwrap();
    let capture = |name: &str, width: u32, height: u32| {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.window().request_redraw();
        let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width as usize);
        });
        elapsed.set(elapsed.get() + Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        ui.window().request_redraw();
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width as usize);
        }));
        assert!(pixels.iter().any(|pixel| pixel.r != pixel.g));
        save_bitmap(
            &directory.join(format!("{name}-{width}x{height}.bmp")),
            width,
            height,
            &pixels,
        );
    };
    let click = |x, y| {
        use slint::platform::{PointerEventButton, WindowEvent};
        let position = slint::LogicalPosition::new(x, y);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    for dark in [false, true] {
        #[cfg(target_os = "windows")]
        i_slint_core::window::WindowInner::from_pub(ui.window())
            .context()
            .set_color_scheme(if dark {
                i_slint_core::items::ColorScheme::Dark
            } else {
                i_slint_core::items::ColorScheme::Light
            });
        let suffix = if dark { "-dark" } else { "" };
        app.set_texts(ModelRc::new(VecModel::from(rows.clone())));
        for (tab, name) in [
            (1, "workbench"),
            (0, "recognition"),
            (2, "patch"),
            (3, "settings"),
        ] {
            app.set_tab(tab);
            for (width, height) in [(1420, 900), (1120, 720), (1920, 1080)] {
                capture(&format!("{name}{suffix}"), width, height);
            }
        }
        app.set_selected_texts(0);
        app.set_texts(ModelRc::new(VecModel::from(
            rows.iter()
                .cloned()
                .map(|mut row| {
                    row.selected = false;
                    row
                })
                .collect::<Vec<_>>(),
        )));
        app.set_tab(1);
        for (width, height) in [(1120, 720), (1420, 900)] {
            capture(&format!("workbench-unselected{suffix}"), width, height);
        }
        app.set_selected_texts(1);
        let mut combined = rows.clone();
        combined.rotate_left(3);
        app.set_texts(ModelRc::new(VecModel::from(combined)));
        app.set_tab(1);
        capture(&format!("workbench-states{suffix}"), 1120, 720);
        app.set_busy(true);
        app.set_task_operation("translate".into());
        app.set_activity("正在翻译所选文本 · 12 / 24".into());
        app.set_task_message("正在翻译 scenario/chapter01.ks；保留角色占位符和换行结构。".into());
        app.set_task_api_summary("翻译 API 链：主翻译接口 · 本地测试配置".into());
        app.set_task_usage(TokenDisplay {
            text: "Token · 输入 12.4K · 输出 2.1K · 缓存 8.2K".into(),
            exact: "输入 12,400 · 输出 2,100 · 缓存 8,200".into(),
        });
        app.set_progress(0.5);
        capture(&format!("workbench-running{suffix}"), 1120, 720);
        click(1052.0, 492.0);
        capture(&format!("workbench-task-details{suffix}"), 1120, 720);
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(900.0, 70.0),
                delta_x: 0.0,
                delta_y: -1000.0,
            });
        capture(
            &format!("workbench-task-details-scrolled{suffix}"),
            1120,
            720,
        );
        click(1052.0, 370.0);
        app.set_tab(0);
        app.set_recognition_running(true);
        app.set_adaptation_running(true);
        app.set_task_operation("adapt".into());
        app.set_status("智能适配中".into());
        app.set_activity("".into());
        app.set_task_message("正在检查脚本结构与原文字节回填。".into());
        app.set_adaptation_progress("正在检查脚本结构与原文字节回填。".into());
        app.set_task_api_summary("分析 API：本地测试配置".into());
        capture(&format!("recognition-running{suffix}"), 1120, 720);
        app.set_modal("engine-review".into());
        capture(&format!("engine-review{suffix}"), 1120, 720);
        app.set_modal("".into());
        app.set_recognition_running(false);
        app.set_adaptation_running(false);
        app.set_tab(1);
        app.set_task_operation("translate".into());
        app.set_status("正在翻译".into());
        app.set_activity("".into());
        app.set_task_message("正在翻译对白。".into());
        app.set_review_running(true);
        app.set_review_message("正在整理同类引擎的验证经验。".into());
        let projects = app.get_projects();
        let initial = projects.row_data(0).unwrap();
        let mut row = initial.clone();
        row.status = app.get_status();
        row.running = true;
        row.review_running = true;
        projects.set_row_data(0, row.clone());
        capture(
            &format!("workbench-translation-and-review{suffix}"),
            1120,
            720,
        );
        app.set_busy(false);
        app.set_tab(0);
        row.running = false;
        projects.set_row_data(0, row);
        capture(&format!("recognition-reviewing{suffix}"), 1120, 720);
        projects.set_row_data(0, initial);
        app.set_review_running(false);
        app.set_review_message("".into());
        app.set_busy(false);
        app.set_activity("".into());
        app.set_page(1);
        app.set_recognition_failure_title("资源覆盖尚未确认".into());
        app.set_recognition_failure_message(
            "主文本脚本尚未通过完整性验收。现有译文与校对数据已保留，可返回识别页查看具体原因。"
                .into(),
        );
        capture(&format!("global-failure{suffix}"), 1120, 720);
        app.set_page(0);
        app.set_tab(0);
        capture(&format!("recognition-failed{suffix}"), 1120, 720);
        app.set_recognition_failure_message("".into());
        app.set_recognition_ready(false);
        app.set_adaptation_pending_coverage("图片中的文字和运行时生成的菜单尚未覆盖。".into());
        app.set_adaptation_runtime_checks("需要在游戏内检查对话换行与字体显示。".into());
        capture(&format!("recognition-pending{suffix}"), 1120, 720);
        let project_name = app.get_project_name();
        app.set_project_name(
            "星海旅人 · 穿越星空与海洋的漫长旅途 · 完整汉化项目与扩展章节合集".into(),
        );
        capture(&format!("recognition-long-title{suffix}"), 1120, 720);
        app.set_project_name(project_name);
        app.set_adaptation_pending_coverage("".into());
        app.set_adaptation_runtime_checks("".into());
        app.set_recognition_ready(true);
        app.set_tab(1);
        app.set_texts(ModelRc::new(VecModel::default()));
        app.set_text_total(0);
        app.set_shown(0);
        app.set_selected_texts(0);
        app.set_editor_id("".into());
        app.set_editor_source("".into());
        app.set_editor_text("".into());
        app.set_editor_status("".into());
        app.set_editor_reviewed(false);
        capture(&format!("workbench-empty{suffix}"), 1120, 720);
        app.set_editor_id("line-1".into());
        app.set_editor_source("ここは私たちの小さな書店です。".into());
        app.set_editor_text("这里是我们的小书店。".into());
        app.set_editor_status("已保存".into());
        app.set_editor_reviewed(true);
        app.set_text_total(12840);
        app.set_shown(5);
        app.set_selected_texts(1);
        app.set_tab(3);
        app.set_modal_title("自动汉化设置".into());
        app.set_modal("workflow-settings".into());
        app.set_workflow_finish(2);
        capture(&format!("workflow-settings{suffix}"), 1120, 720);
        app.set_tab(0);
        app.set_modal_title("识别与适配".into());
        app.set_modal("adapt".into());
        app.set_adaptation_objective(3);
        app.set_workflow_finish(1);
        app.set_workflow_use_existing_text(true);
        capture(&format!("workflow-run{suffix}"), 1120, 720);
        app.set_tab(2);
        app.set_modal("game-check".into());
        app.set_check_scene(1);
        capture(&format!("workflow-scene{suffix}"), 1120, 720);
        app.set_modal("".into());
        app.set_page(2);
        app.set_engine_name("QLIE PACK 3.0 + ImoScripter FormatType=1".into());
        app.set_engine_tab(3);
        let entry = NamedRow {
            id: "filepack".into(),
            name: "QLIE FilePack 3.0 封包和 ImoScripter FormatType=1 脚本可以复用同一套提取与回填流程。中文试译需要核对 CP932 私用码位、字体选择和配套启动器。".into(),
            detail: "已启用 · 0 个工具".into(),
            enabled: true,
        };
        app.set_knowledge_id("".into());
        app.set_knowledge(ModelRc::new(VecModel::from(vec![entry.clone()])));
        capture(&format!("engine-knowledge{suffix}"), 1420, 900);
        app.set_knowledge_id(entry.id.clone());
        app.set_knowledge_enabled(true);
        app.set_knowledge(ModelRc::new(VecModel::from(vec![
            entry,
            NamedRow {
                id: "font".into(),
                name: "核对字体与转区配置。".into(),
                detail: "已停用 · 1 个工具".into(),
                enabled: false,
            },
        ])));
        capture(&format!("engine-knowledge-selected{suffix}"), 1120, 720);
        app.set_page(0);
    }
}
