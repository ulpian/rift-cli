//! The family's recorded identity, measured against the rule it was minted by.
//!
//! The manifest is read, never re-derived, on every product path: that is what
//! lets a repository move and keep its data. The cost is that nothing on those
//! paths notices when the recorded ids and the ids the main checkout would
//! derive now part company, and the dangerous case — a store already filed
//! under the derived ids beside the recorded one — splits one family's data in
//! two without a word. This leg derives and compares.
//!
//! **It reads only.** The manifest is resolved under the no-mint policy, no
//! store is opened (an existence check names the split), and the repair is
//! printed, never run, with or without `--fix`: `rift family remint` is the one
//! writer of the manifest after the first mint, and an identity that changes
//! as a side effect of a diagnosis is not a record. A module of its own
//! because it is the doctor's one reader of the family manifest.

use std::path::{Path, PathBuf};

use rift_core::RepoId;
use rift_git::{Family, FamilyState, MintPolicy, RepoPaths};

/// The rule a family's ids were minted by, and so the rule they are
/// re-derived by: a family that records a main work tree adopted that
/// checkout's path-derived ids; one that records none derived its ids from the
/// canonical common directory under the work-tree-less domains.
#[derive(Clone, Copy)]
enum Rule {
    AdoptMain,
    Fresh,
}

/// One pair of ids and the two stores they key under this machine's cache
/// root.
struct Keyed {
    family_id: RepoId,
    recall_id: RepoId,
    projection: PathBuf,
    recall: PathBuf,
}

/// Why the comparison cannot be made from this vantage. Each is reported as
/// what to run and from where, never as a guess at the answer.
#[derive(Clone, Copy)]
enum Unprovable {
    NoManifest,
    NoProvableMain,
    CommonDirUnreadable,
}

/// The comparison's verdict, once both pairs of ids are in hand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drift {
    Matches,
    Moved,
    Split,
}

impl Rule {
    fn of(family: &Family) -> Self {
        match family.main_work_tree {
            Some(_) => Self::AdoptMain,
            None => Self::Fresh,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AdoptMain => "adopt-main",
            Self::Fresh => "fresh",
        }
    }

    /// What derives the ids under this rule, as the report names it.
    fn source(self) -> &'static str {
        match self {
            Self::AdoptMain => "the main work tree",
            Self::Fresh => "the common directory",
        }
    }
}

impl Keyed {
    fn new(family_id: RepoId, recall_id: RepoId) -> Self {
        let root = crate::family::cache_root();
        Self {
            projection: rift_store::projection_db_under(&root, &family_id),
            recall: rift_store::recall_db_under(&root, &recall_id),
            family_id,
            recall_id,
        }
    }

    fn same_ids(&self, other: &Self) -> bool {
        self.family_id == other.family_id && self.recall_id == other.recall_id
    }

    fn has_store(&self) -> bool {
        self.projection.exists() || self.recall.exists()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "family_id": self.family_id.as_str(),
            "recall_id": self.recall_id.as_str(),
            "projection": self.projection.display().to_string(),
            "projection_exists": self.projection.exists(),
            "recall": self.recall.display().to_string(),
            "recall_exists": self.recall.exists(),
        })
    }

    fn ids_line(&self, label: &str) -> String {
        format!(
            "\n    {label:<9} projection {}  recall {}",
            self.family_id, self.recall_id
        )
    }

    fn paths_lines(&self) -> String {
        format!(
            "\n      {}\n      {}",
            self.projection.display(),
            self.recall.display()
        )
    }
}

impl Unprovable {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoManifest => "no-manifest",
            Self::NoProvableMain => "main-unprovable",
            Self::CommonDirUnreadable => "common-dir-unreadable",
        }
    }

    fn sentence(self, repo: &Path) -> String {
        match self {
            Self::NoManifest => "no recorded identity yet; run `rift init` from the main work \
                                 tree of this repository, the vantage that can prove it"
                .to_owned(),
            Self::NoProvableMain => format!(
                "the main work tree cannot be proven from {}; run `rift doctor` from the \
                 main work tree to compare the recorded ids",
                repo.display()
            ),
            Self::CommonDirUnreadable => format!(
                "the git common directory cannot be read from {}",
                repo.display()
            ),
        }
    }
}

impl Drift {
    fn as_str(self) -> &'static str {
        match self {
            Self::Matches => "matches",
            Self::Moved => "moved",
            Self::Split => "split",
        }
    }
}

/// The recorded identity against the one the minting rule derives now:
/// silent when they match, a line when the repository moved, the remint
/// command when a store under the derived ids splits the family's data.
pub(super) fn doctor_family_identity(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (
            String::new(),
            serde_json::json!({ "status": "unprovable", "reason": "not-a-repository" }),
        );
    };
    let family = match rift_git::resolve_family(&paths, MintPolicy::Never) {
        Ok(FamilyState::Minted(family)) => family,
        Ok(FamilyState::Unminted(_)) => return unprovable_report(repo, Unprovable::NoManifest),
        Ok(FamilyState::Corrupt(manifest)) => return corrupt_report(&manifest),
        Err(_) => return unprovable_report(repo, Unprovable::CommonDirUnreadable),
    };
    let rule = Rule::of(&family);
    let Some(derived) = derive(&paths, rule) else {
        return unprovable_report(repo, Unprovable::NoProvableMain);
    };
    let recorded = Keyed::new(family.family_id.clone(), family.recall_id.clone());
    comparison_report(&family, rule, &recorded, &derived)
}

/// The ids `rule` derives from this vantage, by the derivations the mint
/// adopts from: the per-path ids `rift-store` keeps for adoption and the
/// doctor, or the work-tree-less slug over the canonical common directory.
/// `None` when the main work tree cannot be proven, by the same proof the
/// mint runs.
fn derive(paths: &RepoPaths, rule: Rule) -> Option<Keyed> {
    use rift_core::id::legacy::{bare_slug, BARE_CAPTURE_DOMAIN, BARE_RECALL_DOMAIN};
    let (family_id, recall_id) = match rule {
        Rule::AdoptMain => {
            let main = rift_git::family_members(paths)
                .into_iter()
                .find(|member| member.is_main)?;
            (
                rift_store::capture_repo_id(&main.work_tree).ok()?,
                rift_store::recall_repo_id(&main.work_tree).ok()?,
            )
        }
        Rule::Fresh => {
            let common_dir = std::fs::canonicalize(&paths.common_dir).ok()?;
            (
                RepoId::new(bare_slug(BARE_CAPTURE_DOMAIN, &common_dir)).ok()?,
                RepoId::new(bare_slug(BARE_RECALL_DOMAIN, &common_dir)).ok()?,
            )
        }
    };
    Some(Keyed::new(family_id, recall_id))
}

fn comparison_report(
    family: &Family,
    rule: Rule,
    recorded: &Keyed,
    derived: &Keyed,
) -> (String, serde_json::Value) {
    let drift = match (recorded.same_ids(derived), derived.has_store()) {
        (true, _) => Drift::Matches,
        (false, false) => Drift::Moved,
        (false, true) => Drift::Split,
    };
    let human = match drift {
        Drift::Matches => String::new(),
        Drift::Moved => moved_lines(family, rule, recorded, derived),
        Drift::Split => split_lines(family, rule, recorded, derived),
    };
    let json = serde_json::json!({
        "status": drift.as_str(),
        "minted_from": family.minted_from.as_str(),
        "rule": rule.as_str(),
        "manifest": family.manifest_path.display().to_string(),
        "recorded": recorded.json(),
        "derived": derived.json(),
        "reminted": false,
    });
    (human, json)
}

fn moved_lines(family: &Family, rule: Rule, recorded: &Keyed, derived: &Keyed) -> String {
    let mut human = format!(
        "\n  family identity: moved — the ids recorded at {} ({}) differ from the ids \
         {} derives now, and no store exists under the derived ones",
        family.manifest_path.display(),
        family.minted_from,
        rule.source(),
    );
    human.push_str(&recorded.ids_line("recorded"));
    human.push_str(&derived.ids_line("derived"));
    human.push_str(
        "\n    the manifest travelled with the git directory and the data stays under \
         the recorded ids; nothing to do",
    );
    human
}

fn split_lines(family: &Family, rule: Rule, recorded: &Keyed, derived: &Keyed) -> String {
    let mut human = format!(
        "\n  family identity: SPLIT — data is being filed in two places: under the ids \
         recorded at {} ({}) and under the ids {} derives now",
        family.manifest_path.display(),
        family.minted_from,
        rule.source(),
    );
    human.push_str(&recorded.ids_line("recorded"));
    human.push_str(&recorded.paths_lines());
    human.push_str(&derived.ids_line("derived"));
    human.push_str(&derived.paths_lines());
    let flag = match rule {
        Rule::AdoptMain => "--adopt-main",
        Rule::Fresh => "--fresh",
    };
    human.push_str(&format!(
        "\n    rift doctor never re-mints, --fix included; to record the derived ids:\
         \n      rift family remint {flag} --yes --reason \"…\"\
         \n    the data under the old ids is not moved"
    ));
    human
}

fn unprovable_report(repo: &Path, why: Unprovable) -> (String, serde_json::Value) {
    (
        format!("\n  family identity: unprovable — {}", why.sentence(repo)),
        serde_json::json!({ "status": "unprovable", "reason": why.as_str() }),
    )
}

/// The product path's own refusal, so the doctor and every other reader name
/// an untrustworthy manifest in one sentence.
fn corrupt_report(manifest: &Path) -> (String, serde_json::Value) {
    (
        format!(
            "\n  family identity: {}",
            crate::family::corrupt_manifest_refusal(manifest)
        ),
        serde_json::json!({
            "status": "corrupt",
            "manifest": manifest.display().to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rift_git::{FamilyState, MintPolicy};
    use sha2::{Digest as _, Sha256};

    use super::*;

    /// Mint the family of a fake repository at `root`, as the first product
    /// command run there would, and return its manifest.
    fn minted_repo(root: &Path) -> rift_git::Family {
        rift_git::test_support::fake_repo(root).unwrap();
        let paths = rift_git::discover(root).unwrap();
        match rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap() {
            FamilyState::Minted(family) => family,
            other => panic!("expected a mint, got {other:?}"),
        }
    }

    /// A repository minted at `<tmp>/before` and then moved, `.git` and all,
    /// to `<tmp>/after`: the manifest travelled, and the main work tree now
    /// derives other ids. Returns the new root and the recorded family.
    fn moved_repo(tmp: &Path) -> (PathBuf, rift_git::Family) {
        let before = tmp.join("before");
        std::fs::create_dir_all(&before).unwrap();
        let family = minted_repo(&before);
        let after = tmp.join("after");
        std::fs::rename(&before, &after).unwrap();
        (after, family)
    }

    /// The projection the main checkout at `root` would derive now, created
    /// empty under the test process's cache root.
    fn plant_derived_projection(root: &Path) -> PathBuf {
        let id = rift_store::capture_repo_id(root).unwrap();
        let db = rift_store::projection_db_under(&crate::family::cache_root(), &id);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        db
    }

    fn digest(path: &Path) -> Vec<u8> {
        Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    #[test]
    fn a_freshly_minted_family_reads_matches_and_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let family = minted_repo(tmp.path());

        let (human, json) = doctor_family_identity(tmp.path());

        assert_eq!(human, "", "a match is not a finding");
        assert_eq!(json["status"], "matches", "{json}");
        assert_eq!(json["recorded"]["family_id"], family.family_id.as_str());
        assert_eq!(json["minted_from"], "adopted-main-worktree");
    }

    #[test]
    fn a_moved_repository_with_no_store_under_the_derived_ids_reads_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, family) = moved_repo(tmp.path());

        let (human, json) = doctor_family_identity(&root);

        assert_eq!(json["status"], "moved", "{json}");
        assert_eq!(json["recorded"]["family_id"], family.family_id.as_str());
        let derived = rift_store::capture_repo_id(&root).unwrap();
        assert_eq!(json["derived"]["family_id"], derived.as_str(), "{json}");
        assert!(human.contains("family identity: moved"), "{human}");
        assert!(human.contains("nothing to do"), "{human}");
        assert!(!human.contains("remint"), "a move needs no repair: {human}");
    }

    #[test]
    fn a_store_under_the_derived_ids_reads_split_and_names_the_remint_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, family) = moved_repo(tmp.path());
        let planted = plant_derived_projection(&root);

        let (human, json) = doctor_family_identity(&root);

        assert_eq!(json["status"], "split", "{json}");
        assert_eq!(json["derived"]["projection_exists"], true, "{json}");
        assert_eq!(json["reminted"], false);
        assert!(human.contains("family identity: SPLIT"), "{human}");
        assert!(human.contains(family.family_id.as_str()), "{human}");
        assert!(human.contains(&planted.display().to_string()), "{human}");
        assert!(
            human.contains("rift family remint --adopt-main --yes --reason"),
            "{human}"
        );
        assert!(human.contains("not moved"), "{human}");
    }

    #[test]
    fn doctor_fix_on_a_split_family_changes_no_byte_of_the_manifest_or_its_history() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, _) = moved_repo(tmp.path());
        plant_derived_projection(&root);
        let manifest = root.join(".git/rift-family/family.json");
        let history = root.join(".git/rift-family/family.history.jsonl");
        let before = (digest(&manifest), digest(&history));

        let out = crate::doctor::cmd_doctor(&root, true);

        assert_eq!(
            out.json["family_identity"]["status"], "split",
            "{}",
            out.json
        );
        assert!(
            out.human.contains("rift family remint --adopt-main"),
            "{}",
            out.human
        );
        assert_eq!(
            (digest(&manifest), digest(&history)),
            before,
            "a diagnosis rewrote the record it diagnosed"
        );
    }

    #[test]
    fn a_repository_with_no_manifest_reads_unprovable_and_mints_none() {
        let tmp = tempfile::tempdir().unwrap();
        let git = rift_git::test_support::fake_repo(tmp.path()).unwrap();

        let (human, json) = doctor_family_identity(tmp.path());

        assert_eq!(json["status"], "unprovable", "{json}");
        assert_eq!(json["reason"], "no-manifest", "{json}");
        assert!(human.contains("rift init"), "{human}");
        assert!(!git.join("rift-family").exists(), "the leg minted nothing");
    }

    #[test]
    fn a_manifest_that_cannot_be_parsed_reports_the_refusal_and_repairs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let git = rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let manifest = git.join("rift-family").join("family.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "{not json").unwrap();

        let (human, json) = doctor_family_identity(tmp.path());

        assert_eq!(json["status"], "corrupt", "{json}");
        assert!(
            human.contains("cannot be read; nothing was repaired"),
            "{human}"
        );
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), "{not json");
    }
}
