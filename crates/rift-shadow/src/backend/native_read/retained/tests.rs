use std::cell::Cell;

use super::*;
use crate::backend::native_read::shard_header;
use rift_recall_substrate::{Dialect, MemorySubstrate};

struct Changing {
    inner: MemorySubstrate,
    reads: Cell<usize>,
    changed: Cell<bool>,
}
impl SubstrateReader for Changing {
    fn is_local_only(&self) -> bool {
        true
    }
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>> {
        Ok(vec![shard()])
    }
    fn blob(&self, _id: &str) -> Result<Option<Vec<u8>>> {
        Err(unavailable())
    }
    fn read_artifact(&self, id: &str, path: &str) -> Result<Option<Vec<u8>>> {
        self.reads.set(self.reads.get() + 1);
        if self.changed.get() {
            Ok(Some(b"changed second response".to_vec()))
        } else {
            self.inner.read_artifact(id, path)
        }
    }
}

fn shard() -> CheckpointShard {
    CheckpointShard {
        checkpoint_id: "fixture".into(),
        dialect: Dialect::Rift,
        prev: Some("parent".into()),
        ordinal: 7,
    }
}

#[test]
fn retained_reads_never_reopen_a_changed_source_and_wrong_scope_refuses() {
    let source = Changing {
        inner: MemorySubstrate::local()
            .with_artifact("fixture", "metadata.json", br#"{"sessions":["a"]}"#)
            .with_artifact("fixture", "full.jsonl", b"first retained events"),
        reads: Cell::new(0),
        changed: Cell::new(false),
    };
    let header = shard_header(&source, "fixture").unwrap();
    let native = NativeArtifacts::read(&source, "fixture", &header).unwrap();
    let view = RetainedNativeShard::new(&source, &shard(), &header, &native, None).unwrap();
    let before = source.reads.get();
    source.changed.set(true);
    for _ in 0..2 {
        assert_eq!(
            view.read_artifact("fixture", "full.jsonl")
                .unwrap()
                .as_deref(),
            Some(b"first retained events".as_slice())
        );
    }
    assert!(view.read_artifact("other", "full.jsonl").is_err());
    assert!(view.read_artifact("fixture", "unretained.json").is_err());
    assert!(view.blob("arbitrary-object").is_err());
    assert_eq!(source.reads.get(), before);
    assert_eq!(view.checkpoints().unwrap(), vec![shard()]);
}

#[test]
fn missing_and_empty_metadata_stay_distinct_and_state_only_bodies_are_not_claimed() {
    let source = MemorySubstrate::local()
        .with_artifact("fixture", "metadata.json", br#"{"sessions":["a","b"],"tasks":["missing","empty"],"originals":[{"tool_use_id":"state","original":"original_unsealed"}]}"#)
        .with_artifact("fixture", "tasks/empty/metadata.json", b"")
        .with_artifact("fixture", "sessions/1/metadata.json", b"");
    let header = shard_header(&source, "fixture").unwrap();
    let native = NativeArtifacts::read(&source, "fixture", &header).unwrap();
    let view = RetainedNativeShard::new(&source, &shard(), &header, &native, None).unwrap();
    for (path, expected) in [
        ("tasks/missing/metadata.json", None),
        ("tasks/empty/metadata.json", Some(Vec::new())),
        ("sessions/0/metadata.json", None),
        ("sessions/1/metadata.json", Some(Vec::new())),
    ] {
        assert_eq!(view.read_artifact("fixture", path).unwrap(), expected);
    }
    assert!(view
        .read_artifact("fixture", "tool-originals/state.json")
        .is_err());
}

#[test]
fn inconsistent_duplicate_paths_refuse_instead_of_overwriting_retained_bytes() {
    let source = MemorySubstrate::local()
        .with_artifact("fixture", "metadata.json", br#"{"tasks":["same","same"]}"#)
        .with_artifact("fixture", "tasks/same/metadata.json", b"first");
    let header = shard_header(&source, "fixture").unwrap();
    let mut native = NativeArtifacts::read(&source, "fixture", &header).unwrap();
    native.tasks.get_mut(1).unwrap().metadata = b"different".to_vec();
    assert!(RetainedNativeShard::new(&source, &shard(), &header, &native, None).is_err());
}
