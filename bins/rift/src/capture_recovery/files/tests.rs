use super::*;

#[test]
fn hold_record_reads_obey_the_actual_four_kib_cap_plus_one() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().canonicalize().unwrap();
    let namespace = Namespace::open(&path).unwrap();
    let name = OsStr::new("hold");
    std::fs::write(path.join(name), vec![b'x'; 4096]).unwrap();
    let read = read_regular(&namespace, name, 4096).unwrap();
    assert_eq!(read.bytes.len(), 4096);
    let mut changed = false;
    assert!(read_fd_with(&read.file, 4096, &mut |phase| {
        assert!(phase == ReadPhase::BeforeBoundedRead);
        std::fs::write(path.join(name), vec![b'x'; 8192]).unwrap();
        changed = true;
    })
    .is_err());
    assert!(changed);
    let mut view = &read.file;
    assert_eq!(std::io::Seek::stream_position(&mut view).unwrap(), 4097);
    assert!(read_regular(&namespace, name, 4096).is_err());
    assert_eq!(std::fs::read(path.join(name)).unwrap().len(), 8192);
}
