mod common;

use common::*;
use ttskit_core::Save;
use ttskit_diff::{ObjectStatus, render_text};
use ttskit_testutil::{fixture_saves, read_fixture};

fn load(name: &str) -> Save {
    Save::from_slice(&read_fixture(name)).unwrap()
}

#[test]
fn every_fixture_equals_itself() {
    for path in fixture_saves() {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let s = load(&name);
        let d = run_with(&s, &s.clone(), 0.0, true);
        assert!(d.is_empty(), "{name}: {d:?}");
        // Fixtures may contain duplicate GUIDs, which are listed as warnings.
        let text = render_text(&d, true);
        assert!(text.starts_with("no differences\n"), "{name}");
        assert!(text[15..].lines().all(|l| l.starts_with("! duplicate")));
    }
}

#[test]
fn nested_fixture_removal_is_found() {
    let old = load("containers.json");
    let mut new = old.clone();
    new.map_mut().remove("ObjectStates");
    let d = run(&old, &new);
    assert!(!d.objects.is_empty());
    assert!(d.objects.iter().all(|o| o.status == ObjectStatus::Removed));
    assert!(d.objects.iter().any(|o| o.guid == "bag003"));
    // And the reverse reports the same objects as added.
    let d = run(&new, &old);
    assert!(d.objects.iter().all(|o| o.status == ObjectStatus::Added));
}
