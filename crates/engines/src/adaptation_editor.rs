//! File editing tools for the adaptation workspace.
use super::{
    EngineError, Session, text, verification::TestArguments, workflow, workspace_file, write_path,
    written_selection,
};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_EDIT_BYTES: usize = 512 * 1024;

pub(super) fn write_file(
    workspace: &Path,
    state: &mut Session,
    args: &Value,
) -> Result<Value, EngineError> {
    let relative = text(args, "path")?.replace('\\', "/");
    let path = write_path(workspace, &relative)?;
    let content = write_content(workspace, args)?;
    let release_files = written_selection(state, &relative, args)?;
    let test_arguments = match args.get("testArguments") {
        None => None,
        Some(arguments) if relative == "src/adapter.py" => {
            Some(serde_json::to_value(TestArguments::parse(arguments)?)?)
        }
        Some(_) => {
            return Err(EngineError::Protocol(
                "testArguments 只能随 src/adapter.py 登记。".into(),
            ));
        }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let changed = fs::read(&path).ok().as_deref() != Some(content.as_bytes());
    if changed {
        write_source_file(&path, content.as_bytes())?;
        record_edit(state, &relative);
    }
    state.release_files = release_files;
    let registered = test_arguments.is_some();
    if let Some(arguments) = test_arguments {
        workflow::register(state, arguments);
    }
    Ok(json!({
        "path": args["path"],
        "bytes": content.len(),
        "changed": changed,
        "copyFrom": args.get("copyFrom"),
        "testArgumentsRegistered": registered,
        "verificationConfigured": state.workflow.verification_args.is_some(),
        "nextAction": editing_next_action(state, &relative),
    }))
}

fn write_content<'a>(workspace: &Path, args: &'a Value) -> Result<Cow<'a, str>, EngineError> {
    let content = match (args.get("content"), args.get("copyFrom")) {
        (Some(_), None) => Cow::Borrowed(text(args, "content")?),
        (None, Some(_)) => {
            let source = write_path(workspace, text(args, "copyFrom")?)?;
            let mut bytes = Vec::new();
            fs::File::open(source)?
                .take(MAX_EDIT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            Cow::Owned(String::from_utf8(bytes).map_err(|_| {
                EngineError::Protocol("copyFrom 必须是工作区中的 UTF-8 文本文件。".into())
            })?)
        }
        _ => {
            return Err(EngineError::Protocol(
                "content 与 copyFrom 必须且只能提供一项。".into(),
            ));
        }
    };
    if content.len() > MAX_EDIT_BYTES {
        return Err(EngineError::Rejected(
            "单次源码写入过大，请拆分实现。".into(),
        ));
    }
    Ok(content)
}

pub(super) fn replace_text(
    workspace: &Path,
    state: &mut Session,
    args: &Value,
) -> Result<Value, EngineError> {
    let relative = text(args, "path")?.replace('\\', "/");
    let path = write_path(workspace, &relative)?;
    if path.metadata()?.len() > MAX_EDIT_BYTES as u64 {
        return Err(EngineError::Protocol(
            "局部编辑文件不得超过 512 KiB。".into(),
        ));
    }
    let original = fs::read_to_string(&path)?;
    let old = text(args, "oldText")?;
    if old.is_empty() || original.matches(old).count() != 1 {
        return Err(EngineError::Rejected("oldText 必须精确且唯一匹配。".into()));
    }
    let updated = original.replacen(old, text(args, "newText")?, 1);
    if updated.len() > MAX_EDIT_BYTES {
        return Err(EngineError::Protocol(
            "局部编辑结果不得超过 512 KiB。".into(),
        ));
    }
    let changed = updated != original;
    if changed {
        write_source_file(&path, updated.as_bytes())?;
        record_edit(state, &relative);
    }
    Ok(json!({
        "path": args["path"],
        "bytes": updated.len(),
        "changed": changed,
        "nextAction": editing_next_action(state, &relative),
    }))
}

pub(super) fn remove_file(
    workspace: &Path,
    state: &mut Session,
    args: &Value,
) -> Result<Value, EngineError> {
    let relative = text(args, "path")?.replace('\\', "/");
    let path = write_path(workspace, &relative)?;
    if path.exists() {
        fs::remove_file(&path)?;
        prune_empty_parents(workspace, &relative, &path);
    }
    state
        .release_files
        .retain(|file| !workspace_file(&file.path).eq_ignore_ascii_case(&relative));
    state.verified_fingerprint = None;
    Ok(json!({"removed": args["path"]}))
}

fn prune_empty_parents(workspace: &Path, relative: &str, path: &Path) {
    let Some(root_name) = relative.split('/').next() else {
        return;
    };
    let stop = workspace.join(root_name);
    let mut current = path.parent();
    while let Some(directory) = current {
        if directory == stop || !directory.starts_with(&stop) {
            break;
        }
        if fs::remove_dir(directory).is_err() {
            break;
        }
        current = directory.parent();
    }
}

fn record_edit(state: &mut Session, relative: &str) {
    state.verified_fingerprint = None;
    if relative.starts_with("src/") {
        state
            .extra
            .insert("lastImplementationTurn".into(), json!(state.total_turns));
    }
}

fn write_source_file(path: &Path, content: &[u8]) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| EngineError::Protocol("文件路径缺少目录。".into()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content)?;
    temporary
        .persist(path)
        .map_err(|error| EngineError::Io(error.error))?;
    Ok(())
}

fn editing_next_action(state: &Session, relative: &str) -> &'static str {
    if relative.starts_with("work/") && relative.ends_with(".py") {
        "新建或更新探针可直接用 run_engine_python(script, content, reportFile) 合并写入与运行；已保存脚本可直接运行。"
    } else if relative == "src/adapter.py" && state.workflow.verification_args.is_none() {
        "正式入口已保存；用 write_file 的 testArguments 登记 kind/key/name/resourcePatterns，或调用 test_adapter 进入宿主验收。"
    } else if state.workflow.needs_verification {
        "本批编辑完成且所选文件与资源齐备后，宿主将按已登记参数验收。"
    } else {
        "继续已确定的编辑或验收；局部变化用 replace_text，复用已有代码可用 copyFrom。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_file_prunes_empty_subdirectories_without_removing_workspace_roots() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = root.path();
        let nested = workspace.join("src/tools/sub/helper.py");
        let sibling = workspace.join("src/tools/keep.py");
        let probe = workspace.join("work/nested/probe.py");
        fs::create_dir_all(nested.parent().expect("nested")).expect("dirs");
        fs::create_dir_all(probe.parent().expect("probe")).expect("work dir");
        fs::write(&nested, "x = 1\n").expect("nested file");
        fs::write(&sibling, "y = 2\n").expect("sibling file");
        fs::write(&probe, "print(1)\n").expect("probe file");
        let mut state = Session::default();

        remove_file(
            workspace,
            &mut state,
            &json!({"path": "src/tools/sub/helper.py"}),
        )
        .expect("remove nested");
        assert!(!workspace.join("src/tools/sub").exists());
        assert!(sibling.is_file());

        remove_file(workspace, &mut state, &json!({"path": "src/tools/keep.py"}))
            .expect("remove sibling");
        assert!(!workspace.join("src/tools").exists());
        assert!(workspace.join("src").is_dir());
        assert!(
            fs::read_dir(workspace.join("src"))
                .expect("read src")
                .next()
                .is_none()
        );

        remove_file(
            workspace,
            &mut state,
            &json!({"path": "work/nested/probe.py"}),
        )
        .expect("remove work probe");
        assert!(!workspace.join("work/nested").exists());
        assert!(workspace.join("work").is_dir());
    }
}
