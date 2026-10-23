use super::*;

#[test]
fn checksum_contract_refuses_missing_extra_duplicate_and_invalid_entries() {
    let tag = "v1.0.0-alpha";
    let mut complete = String::new();
    for target in artifacts::TARGETS {
        complete.push_str(&format!(
            "{}  {}\n",
            "a".repeat(64),
            artifacts::name(tag, target)
        ));
    }
    assert_eq!(artifacts::parse_checksums(&complete, tag).unwrap().len(), 5);
    assert!(artifacts::parse_checksums("", tag).is_err());
    assert!(artifacts::parse_checksums(
        &format!("{complete}{}", complete.lines().next().unwrap()),
        tag
    )
    .is_err());
    assert!(
        artifacts::parse_checksums(&complete.replace(&"a".repeat(64), &"g".repeat(64)), tag)
            .is_err()
    );
    assert!(artifacts::parse_checksums(&complete, "v1.0.0-alpha.1").is_err());
}

#[test]
fn native_evidence_must_cover_exact_archives_and_tag() {
    let temporary = tempfile::tempdir().unwrap();
    let tag = "v1.0.0-alpha";
    let entries = artifacts::TARGETS
        .iter()
        .map(|t| (artifacts::name(tag, t), "a".repeat(64)))
        .collect();
    assert!(artifacts::admit(temporary.path(), tag, "commit", &entries).is_err());
    for target in artifacts::TARGETS {
        let record = serde_json::json!({"schema":1,"target":target,"version":tag,"commit":"commit",
            "archive_sha256":"a".repeat(64),"checks":["version","help","archive-binary-match"]});
        write(
            &temporary.path().join(format!("{target}.json")),
            record.to_string(),
        )
        .unwrap();
    }
    assert!(artifacts::admit(temporary.path(), tag, "commit", &entries).is_ok());
    assert!(artifacts::admit(temporary.path(), tag, "other-commit", &entries).is_err());
}

#[test]
fn command_options_refuse_unknown_and_duplicate_flags() {
    for args in [
        vec!["x", "--wrong", "value"],
        vec!["x", "--dist"],
        vec!["x", "--dist", "a", "--dist", "b"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert!(Options(&args).validate(1, &["--dist"]).is_err());
    }
}

#[test]
#[cfg(unix)]
fn previews_skip_a_stale_default_and_keep_explicit_binary_selection_strict() {
    let temporary = tempfile::tempdir().unwrap();
    let stale = temporary.path().join("stale");
    let alpha = temporary.path().join("alpha");
    for (path, version) in [(&stale, "0.9.0"), (&alpha, "1.0.0-alpha")] {
        write(path, format!("#!/bin/sh\nprintf 'rift {version}\\n'\n")).unwrap();
        fixture::executable(path).unwrap();
    }
    let candidates = [
        temporary.path().join("missing"),
        stale.clone(),
        alpha.clone(),
    ];
    assert_eq!(
        preview::select_binary(&candidates, "v1.0.0-alpha").unwrap(),
        alpha.canonicalize().unwrap()
    );
    let args = vec![
        "preview".to_owned(),
        "install".to_owned(),
        "--binary".to_owned(),
        stale.display().to_string(),
    ];
    let error = Options(&args).preview_binary().unwrap_err();
    assert!(error.contains("0.9.0") && error.contains("expected rift 1.0.0-alpha"));
    let error = preview::select_binary(&[stale], "v1.0.0-alpha").unwrap_err();
    assert!(error.contains("cargo build --release -p rift"));
}
