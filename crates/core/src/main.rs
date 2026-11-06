use std::{collections::BTreeMap, path::PathBuf};

use clap::{Parser, Subcommand};
use gamel10n_core::{Application, ToolPaths, default_data_directory};
use gamel10n_engines::{
    CancellationToken, EngineService, PackageManifest,
    protocol::AdapterRequest,
    python::PythonAdapter,
    vntextpatch::{FORMATS, VnTextPatch},
};
use gamel10n_translation::files::read_json;

#[derive(Parser)]
#[command(name = "gamel10n", about = "GameL10n 无界面项目与解析工具入口")]
struct Arguments {
    #[arg(long)]
    data_directory: Option<PathBuf>,
    #[arg(long)]
    tools_directory: Option<PathBuf>,
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// 显示已登记项目，不扫描游戏资源。
    Projects {
        #[arg(default_value = "")]
        query: String,
    },
    /// 添加游戏目录，不自动识别。
    Add { directory: PathBuf },
    /// 按指定的内置格式提取已经解包的脚本。
    Extract { project_id: String, format: String },
    /// 生成补丁到项目目录，不安装；译文文件为 ID 到译文的 JSON 字典。
    Build {
        project_id: String,
        translations: PathBuf,
    },
    /// 分页读取已保存的提取结果，不重新识别。
    Texts {
        project_id: String,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// 只读查看当前翻译、校对状态和筛选摘要，不调用模型或写入项目。
    Workbench {
        project_id: String,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value = "all")]
        status: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// 列出原生脚本解析器键。
    Formats,
    /// 只读检查项目元数据及已有提取缓存，输出数量和加载耗时，不调用解析器或模型。
    Audit,
    /// 执行 JSON CLI v1 请求。
    Tool {
        request: PathBuf,
        #[arg(long)]
        python_adapter: Option<PathBuf>,
        /// 校验发布包及其固定依赖后执行。
        #[arg(long, conflicts_with = "python_adapter")]
        package: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let tools = match arguments.tools_directory {
        Some(root) => ToolPaths::new(root),
        None => ToolPaths::for_application()?,
    };
    if let Operation::Formats = arguments.command {
        println!("{}", FORMATS.join("\n"));
        return Ok(());
    }
    if let Operation::Tool {
        request,
        python_adapter,
        package,
    } = arguments.command
    {
        let request: AdapterRequest = read_json(&request)?;
        if request.protocol_version != 1 {
            return Err("不支持的协议版本。".into());
        }
        if request.extra.contains_key("responseFile") {
            return Err("工具入口使用 stdout JSON，请省略 responseFile。".into());
        }
        let cancellation = CancellationToken::new();
        let response = if let Some(package) = package {
            let (manifest, runtime) = PackageManifest::load_runtime(&package)?;
            let handle = manifest.handle(&runtime)?;
            EngineService::new(tools.asset_root(), tools.clone())
                .with_shared_directory(
                    arguments
                        .data_directory
                        .clone()
                        .unwrap_or_else(default_data_directory)
                        .join("extensions/engine-tools"),
                )
                .execute(&handle, &request, &cancellation)
                .await?
        } else if let Some(entry) = python_adapter {
            PythonAdapter::new(&tools, &entry)?
                .execute(&request, &cancellation)
                .await?
        } else {
            VnTextPatch::new(&tools)
                .execute(&request, &cancellation)
                .await?
        };
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    let app = Application::open(
        arguments
            .data_directory
            .unwrap_or_else(default_data_directory),
        &tools,
    )
    .await?;
    match arguments.command {
        Operation::Projects { query } => println!(
            "{}",
            serde_json::to_string_pretty(&app.list_projects(&query).await)?
        ),
        Operation::Add { directory } => println!(
            "{}",
            serde_json::to_string_pretty(&app.add_project(&directory).await?)?
        ),
        Operation::Extract { project_id, format } => println!(
            "已提取 {} 条文本。",
            app.extract_native(&project_id, &format).await?
        ),
        Operation::Build {
            project_id,
            translations,
        } => {
            let translations: BTreeMap<String, String> = read_json(&translations)?;
            println!(
                "{}",
                app.build_native(&project_id, translations).await?.display()
            );
        }
        Operation::Texts {
            project_id,
            query,
            offset,
        } => {
            let page = app.text_page(&project_id, &query, offset, 100).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"total":page.total,"offset":page.offset,"units":page.units})
                )?
            );
        }
        Operation::Workbench {
            project_id,
            query,
            status,
            offset,
            limit,
        } => {
            let page = app
                .workbench_page(
                    &project_id,
                    &gamel10n_core::workbench::WorkbenchFilter {
                        query,
                        status,
                        ..Default::default()
                    },
                    offset,
                    limit,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&page)?);
        }
        Operation::Audit => {
            let mut results = Vec::new();
            let mut failures = 0;
            for project in app.list_projects("").await {
                let started = std::time::Instant::now();
                let result = async {
                    let metadata = app.open_project(&project.id).await?;
                    let page = app.text_page(&project.id, "", 0, 100).await?;
                    let cold_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let cached = std::time::Instant::now();
                    let again = app.text_page(&project.id, "", 0, 100).await?;
                    let cached_ms = cached.elapsed().as_secs_f64() * 1000.0;
                    let expected = metadata
                        .extra
                        .get("analysis")
                        .and_then(|analysis| analysis.get("unitCount"))
                        .and_then(serde_json::Value::as_u64);
                    let consistent = page.total == again.total
                        && page.units == again.units
                        && expected.is_none_or(|count| count as usize == page.total);
                    Ok::<_, gamel10n_core::CoreError>((page.total, cold_ms, cached_ms, consistent))
                }
                .await;
                match result {
                    Ok((units, cold_ms, cached_ms, consistent)) => {
                        if !consistent {
                            failures += 1;
                        }
                        results.push(serde_json::json!({"projectId":project.id,"units":units,"coldMs":cold_ms,"cachedMs":cached_ms,"consistent":consistent}));
                    }
                    Err(error) => {
                        failures += 1;
                        results.push(
                            serde_json::json!({"projectId":project.id,"error":error.to_string()}),
                        );
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"projects":results.len(),"failures":failures,"results":results})
                )?
            );
            if failures > 0 {
                return Err(format!("{failures} 个项目未通过只读兼容检查。").into());
            }
        }
        Operation::Formats | Operation::Tool { .. } => {}
    }
    Ok(())
}
