use ttskit_cloud::{Call, CloudBackend, CloudError, CloudFile, MemoryCloud, Op, Quota, UgcHandle};

fn not_found(name: &str) -> CloudError {
    CloudError::NotFound {
        name: name.to_owned(),
    }
}

fn backend_err(msg: &str) -> CloudError {
    CloudError::Backend(msg.to_owned())
}

fn call(op: Op, name: Option<&str>) -> Call {
    Call {
        op,
        name: name.map(str::to_owned),
    }
}

#[test]
fn write_read_list_round_trip() {
    let mut cloud = MemoryCloud::new();
    cloud.write("a.bin", b"hello").unwrap();
    assert_eq!(cloud.read("a.bin").unwrap(), b"hello");
    assert_eq!(
        cloud.list().unwrap(),
        [CloudFile {
            name: "a.bin".into(),
            size: 5
        }]
    );
}

#[test]
fn replace_updates_size() {
    let mut cloud = MemoryCloud::new().with_file("a", b"12345".to_vec());
    cloud.write("a", b"12").unwrap();
    assert_eq!(cloud.list().unwrap()[0].size, 2);
    assert_eq!(cloud.read("a").unwrap(), b"12");
}

#[test]
fn case_insensitive_replace_keeps_first_spelling() {
    let mut cloud = MemoryCloud::new();
    cloud.write("A.png", b"one").unwrap();
    cloud.write("a.PNG", b"two").unwrap();
    let list = cloud.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "A.png");
    assert_eq!(cloud.read("a.png").unwrap(), b"two");
    assert_eq!(cloud.read("A.PNG").unwrap(), b"two");
}

#[test]
fn list_is_sorted_by_name() {
    let mut cloud = MemoryCloud::new();
    for name in ["c", "B", "a", "D"] {
        cloud.write(name, b"x").unwrap();
    }
    let names: Vec<String> = cloud.list().unwrap().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["a", "B", "c", "D"]);
    assert_eq!(cloud.file_names(), ["a", "B", "c", "D"]);
}

#[test]
fn quota_exactly_at_limit_is_ok_and_one_over_fails() {
    let mut cloud = MemoryCloud::new().with_quota(10);
    cloud.write("a", &[0; 6]).unwrap();
    cloud.write("b", &[0; 4]).unwrap();
    assert_eq!(
        cloud.quota().unwrap(),
        Quota {
            total: 10,
            available: 0
        }
    );

    let mut cloud = MemoryCloud::new().with_quota(10);
    cloud.write("a", &[0; 6]).unwrap();
    let err = cloud.write("b", &[0; 5]).unwrap_err();
    assert_eq!(
        err,
        CloudError::QuotaExceeded {
            name: "b".into(),
            needed: 5,
            available: 4
        }
    );
    assert_eq!(cloud.file_names(), ["a"]);
    assert_eq!(cloud.quota().unwrap().available, 4);
}

#[test]
fn failed_replace_leaves_old_content() {
    let mut cloud = MemoryCloud::new().with_quota(10).with_file("a", [1; 6]);
    let err = cloud.write("A", &[2; 11]).unwrap_err();
    assert_eq!(
        err,
        CloudError::QuotaExceeded {
            name: "A".into(),
            needed: 5,
            available: 4
        }
    );
    assert_eq!(cloud.contents("a").unwrap(), vec![1; 6]);
}

#[test]
fn replacing_a_big_file_frees_its_quota() {
    let mut cloud = MemoryCloud::new().with_quota(10).with_file("a", [0; 10]);
    cloud.write("a", &[0; 10]).unwrap();
    cloud.write("A", &[0; 3]).unwrap();
    assert_eq!(cloud.quota().unwrap().available, 7);
}

#[test]
fn max_file_size_is_enforced() {
    let mut cloud = MemoryCloud::new().with_max_file_size(4);
    cloud.write("a", &[0; 4]).unwrap();
    let err = cloud.write("b", &[0; 5]).unwrap_err();
    assert_eq!(
        err,
        CloudError::FileTooLarge {
            name: "b".into(),
            size: 5,
            max: 4
        }
    );
    assert_eq!(cloud.file_names(), ["a"]);
}

#[test]
fn empty_name_is_invalid() {
    let mut cloud = MemoryCloud::new();
    let err = cloud.write("", b"x").unwrap_err();
    assert!(matches!(err, CloudError::InvalidName { .. }));
    assert!(cloud.file_names().is_empty());
}

#[test]
fn missing_file_is_not_found() {
    let mut cloud = MemoryCloud::new();
    assert_eq!(cloud.read("Nope").unwrap_err(), not_found("Nope"));
    assert_eq!(cloud.delete("Nope").unwrap_err(), not_found("Nope"));
    assert_eq!(cloud.share("Nope").unwrap_err(), not_found("Nope"));
}

#[test]
fn delete_removes_file_and_frees_space() {
    let mut cloud = MemoryCloud::new().with_quota(10).with_file("a", [0; 7]);
    cloud.delete("A").unwrap();
    assert!(cloud.list().unwrap().is_empty());
    assert_eq!(cloud.quota().unwrap().available, 10);
    assert_eq!(cloud.read("a").unwrap_err(), not_found("a"));
}

#[test]
fn share_is_stable_until_the_file_is_written() {
    let mut cloud = MemoryCloud::new().with_file("a", b"1".to_vec());
    cloud.write("b", b"2").unwrap();
    let first = cloud.share("a").unwrap();
    assert_ne!(first, UgcHandle(0));
    assert_eq!(cloud.share("A").unwrap(), first);
    assert_eq!(cloud.handle("a"), Some(first));

    let other = cloud.share("b").unwrap();
    assert_ne!(other, first);

    cloud.write("a", b"new").unwrap();
    assert_eq!(cloud.handle("a"), None);
    let renewed = cloud.share("a").unwrap();
    assert_ne!(renewed, first);
    assert_ne!(renewed, other);
}

#[test]
fn delete_removes_the_handle() {
    let mut cloud = MemoryCloud::new().with_file("a", b"1".to_vec());
    let first = cloud.share("a").unwrap();
    cloud.delete("a").unwrap();
    cloud.write("a", b"1").unwrap();
    assert_ne!(cloud.share("a").unwrap(), first);
}

#[test]
fn fail_once_on_write_then_success() {
    let mut cloud = MemoryCloud::new();
    cloud.fail_once(Op::Write, Some("a"), backend_err("boom"));
    assert_eq!(cloud.write("A", b"x").unwrap_err(), backend_err("boom"));
    assert!(cloud.file_names().is_empty());
    cloud.write("A", b"x").unwrap();
    assert_eq!(cloud.contents("a").unwrap(), b"x");
}

#[test]
fn persistent_failure_is_scoped_to_one_name() {
    let mut cloud = MemoryCloud::new();
    cloud.fail(Op::Write, Some("bad"), backend_err("no"));
    for _ in 0..3 {
        assert!(cloud.write("BAD", b"x").is_err());
    }
    cloud.write("good", b"x").unwrap();
    assert_eq!(cloud.file_names(), ["good"]);

    cloud.clear_failures();
    cloud.write("bad", b"x").unwrap();
}

#[test]
fn fail_times_counts_down() {
    let mut cloud = MemoryCloud::new();
    cloud.fail_times(Op::Write, None, backend_err("x"), 2);
    assert!(cloud.write("a", b"1").is_err());
    assert!(cloud.write("b", b"1").is_err());
    cloud.write("c", b"1").unwrap();
    assert_eq!(cloud.file_names(), ["c"]);
}

#[test]
fn earliest_matching_injection_wins() {
    let cloud = MemoryCloud::new();
    cloud.fail_once(Op::Read, Some("a"), backend_err("first"));
    cloud.fail(Op::Read, None, backend_err("second"));
    assert_eq!(cloud.read("a").unwrap_err(), backend_err("first"));
    assert_eq!(cloud.read("a").unwrap_err(), backend_err("second"));
}

#[test]
fn injection_on_read_through_shared_reference() {
    let cloud = MemoryCloud::new().with_file("a", b"1".to_vec());
    let shared: &MemoryCloud = &cloud;
    shared.fail_once(Op::Read, Some("a"), backend_err("read"));
    assert_eq!(shared.read("a").unwrap_err(), backend_err("read"));
    assert_eq!(shared.read("a").unwrap(), b"1");
}

#[test]
fn injection_on_list_and_quota_matches_only_none() {
    let cloud = MemoryCloud::new();
    cloud.fail(Op::List, Some("a"), backend_err("named"));
    assert!(cloud.list().is_ok());
    cloud.fail_once(Op::List, None, backend_err("list"));
    assert_eq!(cloud.list().unwrap_err(), backend_err("list"));
    assert!(cloud.list().is_ok());

    cloud.fail_once(Op::Quota, None, backend_err("quota"));
    assert_eq!(cloud.quota().unwrap_err(), backend_err("quota"));
    assert!(cloud.quota().is_ok());
}

#[test]
fn injected_failure_leaves_state_unchanged() {
    let mut cloud = MemoryCloud::new().with_file("a", b"old".to_vec());
    let handle = cloud.share("a").unwrap();
    cloud.fail(Op::Write, None, backend_err("w"));
    cloud.fail(Op::Delete, None, backend_err("d"));
    cloud.fail(Op::Share, None, backend_err("s"));
    assert!(cloud.write("a", b"new").is_err());
    assert!(cloud.delete("a").is_err());
    assert!(cloud.share("b").is_err());
    assert_eq!(cloud.contents("a").unwrap(), b"old");
    assert_eq!(cloud.handle("a"), Some(handle));
}

#[test]
fn call_log_records_failed_calls_and_filters_mutating() {
    let mut cloud = MemoryCloud::new().with_file("seed", b"1".to_vec());
    assert!(cloud.calls().is_empty());
    cloud.fail_once(Op::Write, None, backend_err("x"));

    let _ = cloud.list();
    let _ = cloud.read("Seed");
    let _ = cloud.write("New", b"1");
    let _ = cloud.write("New", b"1");
    let _ = cloud.share("missing");
    let _ = cloud.quota();
    let _ = cloud.delete("seed");
    // Inspection helpers are not logged.
    cloud.contents("seed");
    cloud.handle("seed");
    cloud.file_names();

    assert_eq!(
        cloud.calls(),
        [
            call(Op::List, None),
            call(Op::Read, Some("Seed")),
            call(Op::Write, Some("New")),
            call(Op::Write, Some("New")),
            call(Op::Share, Some("missing")),
            call(Op::Quota, None),
            call(Op::Delete, Some("seed")),
        ]
    );
    assert_eq!(
        cloud.mutating_calls(),
        [
            call(Op::Write, Some("New")),
            call(Op::Write, Some("New")),
            call(Op::Share, Some("missing")),
            call(Op::Delete, Some("seed")),
        ]
    );
    cloud.clear_calls();
    assert!(cloud.calls().is_empty());
}

fn exercise(backend: &mut dyn CloudBackend) {
    backend.write("x", b"data").unwrap();
    assert_eq!(backend.read("x").unwrap(), b"data");
    assert_eq!(backend.list().unwrap().len(), 1);
    backend.share("x").unwrap();
    backend.delete("x").unwrap();
    assert!(backend.list().unwrap().is_empty());
}

#[test]
fn works_as_mut_dyn_and_boxed_backend() {
    let mut cloud = MemoryCloud::new();
    exercise(&mut cloud);
    assert_eq!(cloud.mutating_calls().len(), 3);

    let mut boxed: Box<dyn CloudBackend> = Box::new(MemoryCloud::default());
    exercise(boxed.as_mut());
}
