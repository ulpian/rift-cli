//! Shared owned native shard bytes and header, used by all CLI passes.

pub(super) use rift_shadow::backend::native_read::{
    shard_header, NativeArtifacts, ShardHeader, TaskArtifactBytes,
};
