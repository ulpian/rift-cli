use super::*;

#[test]
fn a_directory_observed_before_open_cannot_be_reclassified_as_initially_absent() {
    for replacement in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let child = root.join("child");
        std::fs::create_dir(&child).unwrap();
        let parent = Namespace::open(&root).unwrap();
        let mut reached = false;
        let result = open_child_with(&parent, OsStr::new("child"), &child, &mut || {
            reached = true;
            std::fs::rename(&child, root.join("saved-child")).unwrap();
            if replacement {
                std::fs::create_dir(&child).unwrap();
            }
        });
        assert!(reached);
        assert!(result.is_err());
        assert!(root.join("saved-child").exists());
    }
}

#[test]
fn only_initial_not_found_with_a_stable_parent_is_available_absence() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let parent = Namespace::open(&root).unwrap();
    let child = root.join("child");
    assert!(open_child_if_present(&parent, OsStr::new("child"), &child)
        .unwrap()
        .is_none());
    std::fs::create_dir(&child).unwrap();
    assert!(open_child_if_present(&parent, OsStr::new("child"), &child)
        .unwrap()
        .is_some());
    std::fs::remove_dir(&child).unwrap();
    std::os::unix::fs::symlink("missing", &child).unwrap();
    assert!(open_child_if_present(&parent, OsStr::new("child"), &child).is_err());
}
