//! Lightweight checks for selecting a single game's installation directory.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::CoreError;

pub(crate) fn validate(directory: &Path) -> Result<PathBuf, CoreError> {
    let directory = std::path::absolute(directory)?;
    if directory.parent().is_none() {
        return Err(CoreError::InvalidData(
            "不能将磁盘或共享根目录作为游戏项目，请选择具体游戏的安装目录。".into(),
        ));
    }
    if !directory.is_dir() {
        return Err(CoreError::InvalidData("游戏目录不存在。".into()));
    }
    Ok(directory)
}

/// Returns a few immediate children that may be separate games, for an optional warning.
/// This bounded, best-effort check does not establish an engine or complete directory inventory.
pub fn suspected_games(directory: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let directory = validate(directory)?;
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(vec![]);
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut checked = 0;
    let mut games = Vec::new();
    for entry in entries.take(128).flatten() {
        if Instant::now() >= deadline || checked >= 32 {
            break;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() || is_link(&metadata) {
            continue;
        }
        checked += 1;
        if looks_like_game(&entry.path(), deadline) {
            games.push(PathBuf::from(entry.file_name()));
            if games.len() == 3 {
                break;
            }
        }
    }
    if games.len() < 2 {
        games.clear();
    }
    games.sort();
    Ok(games)
}

fn looks_like_game(directory: &Path, deadline: Instant) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut executable = false;
    let mut resources = false;
    for entry in entries.take(128).flatten() {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if is_link(&metadata) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if metadata.is_file() {
            let extension = Path::new(&name)
                .extension()
                .and_then(|value| value.to_str());
            executable |= matches!(extension, Some("exe" | "sh"));
            resources |= matches!(
                extension,
                Some(
                    "xp3"
                        | "pfs"
                        | "aos"
                        | "ain"
                        | "arc"
                        | "ypf"
                        | "paz"
                        | "npa"
                        | "rpa"
                        | "nsa"
                        | "ns2"
                        | "cpk"
                        | "pak"
                        | "pck"
                )
            );
            resources |= matches!(extension, Some("dat" | "bin"))
                && (name.starts_with("data")
                    || name.starts_with("script")
                    || name.starts_with("scenario")
                    || name == "gameexe.dat");
        } else if metadata.is_dir() {
            resources |= name.ends_with("_data") || matches!(name.as_str(), "renpy" | "www");
        }
        if executable && resources {
            return true;
        }
    }
    false
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn importing_disk_root_is_rejected_before_creating_project_data() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().ancestors().last().unwrap();
        let data = temporary.path().join("data");
        let store = crate::projects::ProjectStore::open(data.clone())
            .await
            .unwrap();
        assert!(
            store
                .add(root)
                .await
                .unwrap_err()
                .to_string()
                .contains("根目录")
        );
        assert!(!data.exists());
    }

    #[test]
    fn sibling_game_roots_produce_an_advisory_without_reading_resources() {
        let temporary = tempfile::tempdir().unwrap();
        for (name, resource) in [("one", "data.xp3"), ("two", "data00-00.dat")] {
            let game = temporary.path().join(name);
            fs::create_dir(&game).unwrap();
            fs::write(game.join("game.exe"), []).unwrap();
            fs::write(game.join(resource), []).unwrap();
        }
        assert_eq!(
            suspected_games(temporary.path()).unwrap(),
            [PathBuf::from("one"), PathBuf::from("two")]
        );
        assert!(validate(temporary.path()).is_ok());
    }

    #[test]
    fn multiple_executables_and_one_game_do_not_imply_a_game_collection() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["game", "tools", "redist"] {
            let directory = temporary.path().join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("main.exe"), []).unwrap();
            fs::write(directory.join("settings.exe"), []).unwrap();
        }
        fs::create_dir(temporary.path().join("game/main_Data")).unwrap();
        assert!(suspected_games(temporary.path()).unwrap().is_empty());
    }
}
