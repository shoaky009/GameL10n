use gamel10n_translation::files::{read_json, write_json};
use serde::{Serialize, Serializer, ser::Error};
use serde_json::json;

struct CannotSerialize;

impl Serialize for CannotSerialize {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(S::Error::custom("deliberate write failure"))
    }
}

#[test]
fn failed_serialization_keeps_the_previously_committed_file() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("state.json");
    let original = json!({"completed":42});
    write_json(&file, &original).unwrap();
    assert!(write_json(&file, &CannotSerialize).is_err());
    assert_eq!(read_json::<serde_json::Value>(&file).unwrap(), original);
}

#[test]
fn replacing_json_does_not_leave_backup_or_temporary_files() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("state.json");
    write_json(&file, &json!({"completed":1})).unwrap();
    write_json(&file, &json!({"completed":2})).unwrap();
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    assert_eq!(
        read_json::<serde_json::Value>(&file).unwrap()["completed"],
        2
    );
}
