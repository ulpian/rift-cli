//! `rift redact` — redact secrets/PII from captured transcripts and prompts.

use anyhow::{Context, Result};
use rift_core::checkpoint_store::CheckpointStore;
use rift_redact::{checkpoint_text_targets, redact_file, Masker, Pipeline, RulePack};

use crate::cli::RedactArgs;

/// Run the redact command.
///
/// # Errors
/// Propagates rule-pack, store, and file IO errors.
pub fn run(args: &RedactArgs) -> Result<i32> {
    let mut pipeline = Pipeline::builtin();
    for pack_path in &args.packs {
        let pack = RulePack::load(pack_path)
            .with_context(|| format!("loading rule pack {}", pack_path.display()))?;
        pipeline = pipeline.with_pack(pack);
    }
    let masker = Masker::default();

    let store = CheckpointStore::new(&args.store);
    let ids = if args.checkpoints.is_empty() {
        store.list_ids().context("listing checkpoints in store")?
    } else {
        args.checkpoints.clone()
    };

    if ids.is_empty() {
        println!("no checkpoints found under {}", args.store.display());
        return Ok(0);
    }

    let mut total_findings = 0usize;
    let mut total_written = 0usize;

    for id in &ids {
        for path in checkpoint_text_targets(&store, id, args.session) {
            if !path.exists() {
                continue;
            }
            let outcome = redact_file(&pipeline, &masker, &path, args.dry_run)
                .with_context(|| format!("redacting {}", path.display()))?;
            total_findings += outcome.findings;
            if outcome.written {
                total_written += 1;
            }
            if outcome.findings > 0 {
                let tag = if outcome.written {
                    " (rewritten)"
                } else if args.dry_run {
                    " (dry-run)"
                } else {
                    ""
                };
                println!("{}: {} finding(s){tag}", path.display(), outcome.findings);
            }
        }
    }

    println!(
        "\n{total_findings} finding(s) across {} checkpoint(s); {total_written} file(s) {}",
        ids.len(),
        if args.dry_run {
            "would be rewritten"
        } else {
            "rewritten"
        }
    );
    Ok(0)
}
