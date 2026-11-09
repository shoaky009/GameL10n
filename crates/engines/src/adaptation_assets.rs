//! Bundled parser source references and the original adapter capability catalog.
use crate::{
    AdaptationRequest, CancellationToken, EngineError, EngineService,
    adaptation::{Session, text},
    adaptation_context::{BSSCRIPT_GUIDANCE, BUILTIN_FORMATS, builtin_catalog},
    discovery::resolve_relative,
};
use gamel10n_translation::files::hash_file;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const HELPER: &str = include_str!("../../../adapters/components/gamel10n_host.py");
const KIRIKIRI_REFERENCE: &str = include_str!("../references/Kirikiri_text_encoding.md");
const KAGUYA_REFERENCE: &str = include_str!("../references/Kaguya_MESSAGE_v4_0_runtime.md");
const YURIS_REFERENCE: &str = include_str!("../references/Yuris_text_encoding.md");
const ARTEMIS_REFERENCE: &str = include_str!("../references/Artemis_pfs_overlay.md");
const KAGUYA_WARNING: &str = "KaGuYa MESSAGE v4.0 内置适配器只提供 CP932 脚本提取和回填；可提取不代表已支持中文运行。CP932 无法表示的译文会报错，中文编码、字节遍历和字体绘制仍需验证。";
const GRAPHICS_SHA256: &str = "E1292E3D47D1C65F4AE52A30B1159A476137422BD7A9272A5A2E0129F3CB0684";

pub(crate) fn template_catalog() -> Vec<Value> {
    [
        ("loose-renpy", "Renpy", None, vec![".rpy"], vec!["**/*.rpy"], "Ren’Py 源脚本"),
        ("loose-kirikiri-ks", "KirikiriKs", None, vec![".ks"], vec!["**/*.ks"], "Kirikiri KS 源脚本"),
        ("rpa3-renpy", "Renpy", Some("archive.rpa3"), vec![".rpy"], vec!["**/*.rpa"], "RPA3 + Ren’Py 源脚本"),
        ("loose-renpy-rpyc", "Renpy", None, vec![".rpyc"], vec!["**/*.rpyc"], "Ren’Py RPYC2 编译脚本"),
        ("rpa3-renpy-rpyc", "Renpy", Some("archive.rpa3"), vec![".rpyc"], vec!["**/*.rpa"], "RPA3 + Ren’Py RPYC2 编译脚本"),
        ("renpy-mixed", "Renpy", Some("archive.rpa3"), vec![".rpy", ".rpyc"], vec!["**/*.rpy", "**/*.rpyc", "**/*.rpa"], "Ren’Py 无冲突混合资源"),
        ("xp3-kirikiri-ks", "KirikiriKs", Some("archive.xp3"), vec![".ks"], vec!["**/*.xp3"], "无过滤器 XP3 + Kirikiri KS"),
    ].into_iter().map(|(id,format,archive,extensions,patterns,name)|json!({"id":id,"version":1,"format":format,"archiveComponent":archive,"scriptExtensions":extensions,"resourcePatterns":patterns,"family":{"kind":"template","key":id,"name":name,"adapterId":format!("template.{id}")}})).collect()
}

pub(crate) fn engine_components() -> Vec<Value> {
    vec![
        json!({"id":"archive.rpa3","version":2,"format":"RPA/ARC 3.0 single-chunk","module":"python.archives.rpa","operations":["inspect","extract","rebuild"],"limitations":["RPA 2/4, multi-chunk entries and scrambled archives are unsupported; compiled scripts require the RPYC2 template.","Only a data-only pickle index is accepted; executable pickle operations are rejected."]}),
        json!({"id":"archive.xp3","version":1,"format":"Standard XP3 v1, filter=none","module":"python.archives.xp3","operations":["inspect","extract","rebuild"],"standaloneBuild":{"call":"python.archives.xp3.build([(name, bytes), ...], filter_name='none')","condition":"Use only with established unfiltered loading and override precedence; the writer does not establish either. Call the pinned API from Python, not run_engine_component.","fallback":"Keep replace output when these facts are unavailable; no extra research solely for patch size."},"limitations":["Explicit filter=none is required from independent engine evidence; zero flags do not establish this.","Encrypted/filtered, chained, obfuscated and executable-overlay XP3 are unsupported.","Changed archives use standard v1 layout; game patch precedence and runtime compatibility require verification."]}),
    ]
}

pub(crate) fn siglus_description() -> Value {
    json!({"id":"engine.siglus","version":"1.0.0","name":"Siglus / Scene.pck","upstream":"SiglusSceneScriptUtility 0.5.7 (Unlicense OR 0BSD)",
        "capabilities":["Scene.pck 解包及重封包","内嵌源码查询","字节码参数字符串引用","变长 UTF-16 回填","GDI 字体检查及 FKDS 图片字库补字","独立中文启动入口"],
        "templates":["标准 CD_TEXT/CD_NAME","FKDS 气泡对白"],
        "sources":["gamel10n_siglus.py","siglus_binary.py","siglus_font.py","siglus_ssu/pck.py","siglus_ssu/dat.py","siglus_ssu/g00.py","siglus_ssu/disam.py","siglus_ssu/GEI.py"],
        "usage":concat!("inspect_siglus_game 直接分析当前游戏，返回结构、已识别文本和待处理调用；source/query 按需读取内嵌源码。",
            "copy_adapter id=engine.siglus/version=1.0.0 生成固定依赖和 JSON 调用桥。规则放在 src/tools/siglus-rules.json，",
            "commandRules:[{command:'精确函数名',argument:1,kind:'dialogue'}] 的 argument 从 0 开始。",
            "仅选择源码或已验证语义确认的显示参数；不要按字符串内容猜测引用。test_adapter 仍须通过。"),
        "limits":"引擎版本、显示模板和 EXE 补丁特征分别检查；未识别的调用不自动翻译，游戏实际显示需试译确认。"})
}

fn namespace(format: &str) -> Option<&'static str> {
    match format {
        "AdvHd" => Some("AdvHd"),
        "Age" => Some("ArcGameEngine"),
        "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt" => Some("Artemis"),
        "Ethornell" => Some("Ethornell"),
        "KirikiriKs" | "KirikiriSoc" | "KirikiriTjs" => Some("Kirikiri"),
        "Majiro" => Some("Majiro"),
        "Mware" => Some("Mware"),
        "Propeller" => Some("Propeller"),
        "RealLive" => Some("RealLive"),
        "ShSystem" => Some("ShSystem"),
        "SilkysMap" | "SilkysMes" => Some("Silkys"),
        "Softpal" => Some("Softpal"),
        "SystemNnnDev" | "SystemNnnRelease" => Some("SystemNnn"),
        "TmrHiroAdvSystemCode" | "TmrHiroAdvSystemText" => Some("TmrHiroAdvSystem"),
        "Yuris" => Some("Yuris"),
        _ => None,
    }
}

fn reference(format: &str) -> Option<(&'static str, &'static str)> {
    match format {
        "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt" => {
            Some(("reference/Artemis_pfs_overlay.md", ARTEMIS_REFERENCE))
        }
        "Kaguya" => Some(("reference/Kaguya_MESSAGE_v4_0_runtime.md", KAGUYA_REFERENCE)),
        "Yuris" => Some(("reference/Yuris_text_encoding.md", YURIS_REFERENCE)),
        "KirikiriKs" | "KirikiriSoc" => {
            Some(("reference/Kirikiri_text_encoding.md", KIRIKIRI_REFERENCE))
        }
        _ => None,
    }
}

fn page_source(
    format: &str,
    path: &str,
    content: &str,
    args: &Value,
) -> Result<Value, EngineError> {
    let number = |key: &str, default: u64, min: u64, max: u64| -> Result<usize, EngineError> {
        let value = match args.get(key) {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| EngineError::Protocol(format!("{key} 必须为整数。")))?,
            None => default,
        };
        if !(min..=max).contains(&value) {
            return Err(EngineError::Protocol(format!("{key} 超出允许范围。")));
        }
        Ok(value as usize)
    };
    let units: Vec<_> = content.encode_utf16().collect();
    let start = number("offset", 0, 0, i32::MAX as u64)?.min(units.len());
    let end = (start + number("length", 12000, 1, 24000)?).min(units.len());
    Ok(
        json!({"format":format,"path":path,"offset":start,"length":end-start,"totalLength":units.len(),"nextOffset":end,"truncated":end<units.len(),"content":String::from_utf16_lossy(&units[start..end])}),
    )
}

pub(crate) async fn inspect_builtin(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    state: &Session,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let key = text(args, "format")?;
    if key.eq_ignore_ascii_case("NekoSDK") {
        return Ok(
            json!({"format":key,"status":"reference-only","engine":"nekosdk","usage":"请调用 inspect_engine_reference(engine=nekosdk)。共享 Python 已提供 NEKOPACK4 和 ADVSCRIPT2 工具；尚无经过当前游戏验证的内置中文运行适配器。"}),
        );
    }
    if key.eq_ignore_ascii_case("BSScriptRev6") || key == "reference.bsscript-rev6" {
        return Ok(
            json!({"format":"BSScriptRev6","status":"reference-only","usage":BSSCRIPT_GUIDANCE}),
        );
    }
    let format = BUILTIN_FORMATS
        .iter()
        .find(|format| format.key.eq_ignore_ascii_case(key))
        .ok_or_else(|| EngineError::Protocol(format!("未知内置脚本格式：{key}")))?;
    let key = format.key;
    let parser_type = format!(
        "VNTextPatch.Shared.Scripts.{}{key}Script",
        namespace(key)
            .map(|namespace| format!("{namespace}."))
            .unwrap_or_default()
    );
    let binary = service.tools.vntextpatch();
    let bundle = binary
        .parent()
        .unwrap_or(Path::new("."))
        .join("sources.zip");
    if let Some(path) = args.get("path") {
        let path = path
            .as_str()
            .ok_or_else(|| EngineError::Protocol("path 必须为字符串。".into()))?
            .replace('\\', "/");
        if path == "tools/gamel10n_host.py" {
            return page_source(key, &path, HELPER, args);
        }
        if let Some((reference, content)) =
            reference(key).filter(|(reference, _)| *reference == path)
        {
            return page_source(key, reference, content, args);
        }
        let mut params = args.clone();
        params["operation"] = json!("builtin_sources");
        params["format"] = json!(key);
        params["path"] = json!(path);
        params["bundle"] = json!(bundle);
        params["parserType"] = json!(parser_type);
        return service.helper(&params, cancellation).await;
    }
    let source = service
        .helper(
            &json!({"operation":"builtin_sources","bundle":bundle,"parserType":parser_type}),
            cancellation,
        )
        .await;
    if matches!(source, Err(EngineError::Cancelled)) {
        return Err(EngineError::Cancelled);
    }
    let mut files = source
        .as_ref()
        .ok()
        .and_then(|source| source["sourceFiles"].as_array())
        .cloned()
        .unwrap_or_default();
    files.push(json!({"path":"tools/gamel10n_host.py","bytes":HELPER.len()}));
    if let Some((path, content)) = reference(key) {
        files.push(json!({"path":path,"bytes":content.len()}));
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let issue=source.as_ref().err().map(|error|json!({"code":if bundle.is_file(){"corrupt"}else{"missing"},"asset":"sources.zip","message":error.to_string(),"recovery":"源码包故障不代表引擎不受支持。binaryAvailable=true 时，继续用 try_builtin_adapter 验证兼容性，并以 copy_adapter 复用固定解析器；恢复源码包后可读取结构参考，新增逻辑使用 Python。"}));
    let mut limitations = vec![
        "仅提供已解包脚本的提取、回填和复提校验；资源包的解包/重打包、运行时编码补丁、启动代理及字体支持需要由适配器实现并验证。",
        if matches!(key, "KirikiriKs" | "KirikiriSoc") {
            "KS/SOC 支持读取 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；须验证游戏实际脚本加载路径接受该编码，原文无修改仍保留原字节。"
        } else if key == "Yuris" {
            "默认读写严格 CP932，包括上游名称为 SjisTunnelEncoding 的默认实现。显式 yurisEncoding 可使用固定 sjis-tunnel 映射读写；不自动生成映射、代理或字体，不证明游戏运行兼容。"
        } else {
            "保留内置解析器的原编码约束；Shift-JIS 无法表示的中文会明确报错，不生成未验证的 sjis_ext.bin。"
        },
        "生成适配器的 probe 只获得最多 65536 字节文件头，必须由 Python 按签名、长度和版本识别；调用桥禁止直接使用整文件解析器执行 probe。",
        "解析器源码仅供了解格式；发行包不含 C# 编译器，需要修改的适配逻辑使用 Python，固定解析器通过预编译组件调用。",
    ];
    limitations.push(match key {
        "Kaguya"=>KAGUYA_WARNING,"Renpy"=>"Ren’Py 仅处理单行对白、旁白和菜单；Python、界面及动态表达式不在此解析器范围。",
        "Yuris"=>"YSTB 的指令、属性描述、值、行号区分别重置 XOR 游标；属性描述为 12 字节，类型位于 +2，值长度 +4，值区相对偏移 +8。无属性的脚本不含可提取文本，也无法用首属性推断密钥，应保留原字节。命令和属性含义以当前 ysc.ybn 为准；清点文本请复用 YurisCommandList/YurisScenarioScript，勿把资源路径或变量标识作为显示文字。YPF 容器和中文运行支持仍需单独验证。",
        "KirikiriTjs"|"Mware"=>"编译脚本沿用上游文本识别规则并排除明显资源路径，试译前需要复核变量和表达式。",
        "TmrHiroAdvSystemText"=>"文本表没有引擎签名，只验证记录和文本结构；不能仅凭兼容记录认定游戏引擎。",_=>"解析器版本兼容性与游戏实际加载行为仍须针对当前游戏验证。",
    });
    let guidance = if key == "Kaguya" {
        kaguya_guidance(kaguya_inspection(
            &request.game_directory,
            state,
            cancellation,
        )?)
    } else if matches!(key, "KirikiriKs" | "KirikiriSoc") {
        kirikiri_guidance()
    } else if matches!(key, "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt") {
        json!({
            "patchArchives":"现有证据已确认独立 PFS 覆盖卷与加载优先级时，复用 python.archives.pfs.build 写入修改成员及必要伴随资源，build.mode='overlay'；按原资源＋补丁复提，保留公共文本身份。原文无修改可返回空 files。接入见 reference/Artemis_pfs_overlay.md。未知规则保留 replace，不为体积增加研究、下载、Hook 开发或验收阶段。",
            "naming":"bounded probe 从候选文件名选取同族最大数字卷之后的路径，放入 buildHints，后续构建稳定复用。媒体卷只需名称或索引时不加入 requiredFiles，不复制整包。"
        })
    } else if key == "Yuris" {
        json!({
            "textSources":"需要区分显示文字与内部字段时，调用 parser(request, 'Yuris', operation='extract', includeYurisSources=True)。响应每条 unit 的 yurisSource 提供 groupIndex、role，以及 attributes 中的 commandIndex/command/attributeIndex/attributeId/attribute/attributeType；索引从 0 起，attributeIndex 为脚本属性描述符序号。姓名与正文共享 groupIndex，YSCF 标题为 role=caption、attributes=[]。默认提取省略此字段，ID、locator、context 和文本数量不变。",
            "selection":"复用原生提取及来源信息，在 Python 中一次定位并筛选已确认的显示字段；不要遍历全部字符串常量当作待翻译文字，也无需通过反复注入标记猜测每条文本来源。沿用被保留文本的原 ID、locator 和 context，不按筛选结果重新编号。命令属性含义以当前 ysc.ybn 为准。",
            "roundtrip":"原生回填依据原姓名/正文分组保留边界，支持括号独白的前后缀及整句改写。出现复提差异时，先按诊断中的脚本和 string:N 做局部回填、复提对比，再执行完整 test_adapter；原文逐字节一致、译文准确、未修改项不变均须验证。",
            "encoding":"将原始解包脚本直接交给固定解析器，不准备 Shadow 副本。有修改且需隧道时，先用 sjis-tunnel 组件统一规划映射，再传 yurisEncoding={'source':'cp932','output':'sjis-tunnel','mapping':''.join(tunnel.chars)}；原生不新增槽位，按 output 复提核对。已有隧道输入用 source='sjis-tunnel' 及原映射。接入示例见 reference/Yuris_text_encoding.md。",
            "unchangedBuild":"先校验译文 ID，再剔除等于原文的条目。无实际修改时，直接调用原生 build，source/output 均沿用输入编码及已有映射；跳过新映射规划和全脚本转换，保留原字节与既有伴随文件，不安装新运行组件。封包无成员修改时复制原包，仍执行所需验收。复用一次提取的 yurisSource，勿逐条重新解密脚本。",
            "dependency":"新适配器由 copy_adapter 登记当前解析器；已有适配器继续保留原 pin。旧版本缺少接口时，保留 src、ID 和译文并报告能力缺口，不重建适配器、不重置源码或身份、不手改依赖 hash；仅经受支持的显式依赖更新并重新验收后使用新接口。",
            "runtime":"YPF 容器、字符遍历、中文字体与代理加载仍需验证。inspect_adapter_component(id='vntextproxy-runtime') 提供固定官方代理包、成员、字体规则和准备入口；它不代表当前游戏运行通过。映射必须跨脚本共用且保留已有槽位顺序，不能逐文件重建。"
        })
    } else {
        Value::Null
    };
    let catalog = builtin_catalog()
        .into_iter()
        .find(|format| format["format"] == key)
        .unwrap();
    let usage = format!(
        "在 src/adapter.py 实现协议入口。from tools.gamel10n_host import parser, write_response；parser(request, '{key}') 调用固定解析器并返回响应字典。"
    ) + "先解包到独立工作目录，再传入该目录及相对 resourceFiles；build 的 outputDirectory 必须与 sourceDirectory 分开，随后按已确认的加载规则重封包或生成增量覆盖包，声明所有最终输出。"
        + "仅登记固定版本的共享解析工具，由预编译宿主通过进程协议调用，不复制到项目。解析器不支持的逻辑使用 Python 实现。"
        + "最终响应用 write_response(request, response) 写入 responseFile；诊断写 stderr。probe 由入口独立实现，不能用整文件解析器猜测截断输入。";
    let example = format!(
        "from tools.gamel10n_host import parser, write_response\n# Implement bounded probe separately; this full parser handles extract/build/validate.\nresponse = parser(request, \"{key}\", sourceDirectory=unpacked_scripts_directory,\n                  resourceFiles=script_relative_paths, responseFile=None)\n# Build also needs outputDirectory and translationsFile (or translations).\n# Repack into the game's resource format and return final resource paths before write_response."
    );
    Ok(
        json!({"format":key,"name":format.name,"parserType":parser_type,"upstreamCommit":"e68e174f3db6181e03fc7fabfa7570e1b293d148","extensions":catalog["extensions"],"companionFileNames":catalog["companions"],"operations":["extract","build","validate"],"binaryAvailable":binary.is_file(),"sourceAvailable":source.is_ok(),"sourceBundleIssue":issue,"sourceFiles":files,"limitations":limitations,"usage":usage,"example":example,"engineGuidance":guidance}),
    )
}

fn kirikiri_guidance() -> Value {
    json!({"parserReuse":crate::adaptation_context::KIRIKIRI_SUMMARY,
        "smilProfile":"File/smil 索引先用 xp3-smil 恢复名称并保留原始别名；同名启动成员可能含保护用伪条目。SMIL 不代表某个固定过滤器，须对多个实际成员验证参数及明文校验，再区分 TJS2100、TJS 源码、KS 和 PSB SCN。内置 SMIL/SmileCrypt 配置只接受其有界样本验证通过的变体。",
        "archiveSupport":"XP3 容器、脚本语义、中文编码和字体是不同能力。现有适配器失败时先检查可复制的源码；容器组件拒绝过滤标记或超出预算，不代表 KS 解析器不可复用。TJS 源码与 TJS2100 编译脚本分别处理。",
        "patchArchives":"若现有初始化脚本或已验证的同类运行时资料已明确 patch 卷或裸文件覆盖及搜索优先级，优先复用已有 writer 与加载路径输出修改脚本、配置和必要字体。已掌握过滤器时沿用，不能为做小包改成无过滤器或另加 DLL；仅已确认无过滤器时可用 python.archives.xp3.build。规则或现成能力不明时保留 replace，不为体积另开研究、下载、Hook 开发或额外验收。",
        "minimalTrial":"先选一个短 KS 和含姓名/选项的代表样本：原文往返、少量中文及保留符号（如 ♪、・）、局部修改后复提。比较实际译文、ID、位置和所有未修改项；封包重建另核对修改与未修改条目的清单及内容。小样通过后再完整 test_adapter。",
        "runtimeEvidence":"确认脚本字节缓冲实际到达的解码函数及编码选择条件。导入表、宿主 ACP、几种指令字节未命中或自定义 encoder/decoder 往返，都不能证明游戏的脚本编码或显示支持。",
        "fontLayout":"Unicode 脚本可读写不等于原日文字体覆盖中文。检查实际 userFace/defaultFace、System.addFont、mapPrerenderedFont 及 font/reset/restore 路径；先用 windows-font-metrics 在同高度/宽度/charset 下比较原字体和覆盖中文的候选字体。缺字时 getTextWidth 与 drawText 可能使用不同回退或度量，造成个别字重叠；需让测宽和绘制使用同一套已覆盖的字形。优先使用引擎已有字体配置或集中字体选择点，保留原字号、风格及正常日文/符号/ASCII；不要插空格、改译文、全局加 pitch、缩小全文或套 QLIE 专用度量。核对 chDefaultFace 等系统设置及读档恢复是否覆盖新配置，不删除用户存档；必要时只迁移已确认的原字体值。原文构建仍须逐字节一致；中文构建才按已确认条件修改字体配置，并独立核对配置已写入实际生效资源。预渲染字库/GDI 以外后端须单独检查；独立测量通过仍需试译确认游戏内排版。",
        "referenceSource":"reference/Kirikiri_text_encoding.md"})
}

fn kaguya_guidance(inspection: Value) -> Value {
    json!({"scriptFormat":"[SCR-MESSAGE]ver4.0","parserReuse":"优先复用现有 KaguyaScript / BuiltinScriptParser 的提取、回填和复提校验，不重写 MESSAGE 解析器。仅确认版本或结构不兼容时才编辑源码。",
        "translationMemory":"沿用原提取结果的 UnitId（协议 id）、ResourcePath、Locator、Context、Speaker 和 Kind；包装适配器不得重新编号或按显示类型重新分类，以免已有翻译记忆失效。","runtimeSupport":KAGUYA_WARNING,
        "nextSteps":["在补丁生成时对实际输出的 Unicode 文本统计 CP932 不可编码字符和可用码位；没有译文时先用少量代表中文验证方案，不要求用户先全量翻译。数量随译文变化，不能固化样本缺字数，也不能把映射后的占位字符存回翻译记忆。","F040/F041 在已调查运行库中是玩家姓名控制码；不要分配给中文字形。其他 PUA 码位仍须验证字节遍历、控制码冲突和绘制路径。","先确认实际运行库和 GDI 导入，再协调编码、TextOutA 字节解释与字体方案；仅换字体或把 CP932 改成 CP936 不能证明游戏已支持中文。","未知运行库须沿导入和 TextInitialize/绘制调用链重新定位；静态线索和独立 GDI 字宽测量都不等于游戏内验收。"],
        "runtimeInspection":inspection,"referenceSource":"reference/Kaguya_MESSAGE_v4_0_runtime.md"})
}

fn kaguya_inspection(
    game: &Path,
    state: &Session,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let candidates = state
        .extra
        .get("builtinDiscovery")
        .and_then(|discovery| discovery["candidates"].as_array())
        .into_iter()
        .flatten()
        .filter(|candidate| {
            candidate["format"]
                .as_str()
                .is_some_and(|format| format.eq_ignore_ascii_case("Kaguya"))
        })
        .flat_map(|candidate| candidate["sampleFiles"].as_array().into_iter().flatten());
    let probes = state
        .extra
        .get("builtinProbes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|probe| {
            probe["format"]
                .as_str()
                .is_some_and(|format| format.eq_ignore_ascii_case("Kaguya"))
        })
        .flat_map(|probe| probe["resources"].as_array().into_iter().flatten());
    let mut seen = BTreeSet::new();
    let resources: Vec<_> = candidates
        .chain(probes)
        .filter_map(Value::as_str)
        .filter(|path| seen.insert(path.to_lowercase()))
        .take(8)
        .collect();
    let mut messages = Vec::new();
    let mut directories = Vec::<PathBuf>::new();
    let mut diagnostics = Vec::new();
    let root = game.canonicalize()?;
    for resource in resources {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let result = (|| -> Result<(), EngineError> {
            let path = resolve_relative(&root, resource)?;
            let mut file = fs::File::open(&path)?;
            let mut header = [0; b"[SCR-MESSAGE]ver4.0".len()];
            if file.metadata()?.len() < header.len() as u64 {
                return Ok(());
            }
            file.read_exact(&mut header)?;
            if header != *b"[SCR-MESSAGE]ver4.0" {
                return Ok(());
            }
            messages.push(resource.replace('\\', "/"));
            for directory in path.parent().into_iter().flat_map(Path::ancestors) {
                if !directories.iter().any(|known| {
                    known
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&directory.to_string_lossy())
                }) {
                    directories.push(directory.into());
                }
                if directory == root || directories.len() >= 24 {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result
            && diagnostics.len() < 3
        {
            diagnostics.push(format!("无法检查 MESSAGE 样本：{error}"));
        }
    }
    let mut libraries = Vec::new();
    let mut known = Value::Null;
    let mut seen = BTreeSet::new();
    for directory in directories.iter().take(24) {
        for suffix in ["Graphics.dll", "Lib/Graphics.dll"] {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let candidate = directory.join(suffix);
            if !seen.insert(candidate.to_string_lossy().to_lowercase()) {
                continue;
            }
            let result = (|| -> Result<(), EngineError> {
                let relative = candidate
                    .strip_prefix(&root)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let path = resolve_relative(&root, &relative)?;
                if !path.is_file() {
                    return Ok(());
                }
                if path.metadata()?.len() > 16 * 1024 * 1024 {
                    libraries.push(json!({"path":relative,"status":"size-limit","sha256":null}));
                    return Ok(());
                }
                let hash = hash_file(&path)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?
                    .to_uppercase();
                let matched = !messages.is_empty() && hash == GRAPHICS_SHA256;
                libraries.push(json!({"path":relative,"status":if matched{"known-runtime"}else{"unknown-runtime"},"sha256":hash}));
                if matched {
                    known = json!({"profile":"MESSAGE-v4.0/Graphics-sha256-E1292E3D","graphicsSha256":GRAPHICS_SHA256,"textInitializeChainRva":["TextInitialize","0x3A8E0","0x3AC20"],"crtIsmbbleadCallRva":"0x3AD8A","originalCrtIsmbbleadTargetRva":"0x41248","playerNameControls":["0xF040","0xF041"],"application":"以上 RVA 仅是该原始 SHA256 对应运行库的静态调查线索，不能作为可直接应用的补丁。修改前核对原指令和调用关系；其他 SHA256 必须重新定位。","runtimeStatus":"not-tested"});
                }
                Ok(())
            })();
            if let Err(error) = result
                && diagnostics.len() < 3
            {
                diagnostics.push(format!("无法检查相邻 Graphics.dll：{error}"));
            }
        }
    }
    Ok(
        json!({"status":if !known.is_null(){"known-runtime-static-evidence"}else if !messages.is_empty(){"message40-only"}else{"no-confirmed-message40"},"messageFiles":messages,"graphicsLibraries":libraries,"knownRuntime":known,"diagnostics":diagnostics,"runtimeStatus":"not-tested","scope":"只读取已发现的最多 8 个 Kaguya 样本文件头；在最多 24 个相邻/祖先目录中仅检查 Graphics.dll 和 Lib/Graphics.dll 两个固定路径，不递归搜索，不修改游戏。"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kaguya_inspects_only_discovered_samples_and_neighbor_libraries() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("script")).unwrap();
        fs::write(
            root.path().join("script/message.dat"),
            b"[SCR-MESSAGE]ver4.0",
        )
        .unwrap();
        fs::write(root.path().join("Graphics.dll"), b"unknown runtime").unwrap();
        fs::create_dir_all(root.path().join("unrelated")).unwrap();
        fs::write(
            root.path().join("unrelated/Graphics.dll"),
            b"must not inspect",
        )
        .unwrap();
        let mut state = Session::default();
        state.extra.insert(
            "builtinDiscovery".into(),
            json!({"candidates":[{"format":"Kaguya","sampleFiles":["script/message.dat"]}]}),
        );
        let result = kaguya_inspection(root.path(), &state, &CancellationToken::new()).unwrap();
        assert_eq!(result["status"], "message40-only");
        assert_eq!(result["graphicsLibraries"].as_array().unwrap().len(), 1);
        assert_eq!(result["graphicsLibraries"][0]["path"], "Graphics.dll");
        assert_eq!(result["knownRuntime"], Value::Null);
        assert_eq!(
            fs::read(root.path().join("Graphics.dll")).unwrap(),
            b"unknown runtime"
        );
    }
}
