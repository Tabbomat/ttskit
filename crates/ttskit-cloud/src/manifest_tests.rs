use super::*;
use crate::sha1_hex;

fn sha(data: &str) -> String {
    sha1_hex(data.as_bytes())
}

fn url(n: u64, data: &str) -> String {
    format!(
        "https://steamusercontent-a.akamaihd.net/ugc/{n}/{}/",
        sha(data)
    )
}

fn me(path: Option<&str>, cloud_path: &str, data: &str) -> ManifestEntry {
    ManifestEntry {
        path: path.map(str::to_owned),
        cloud_path: cloud_path.to_owned(),
        sha1: sha(data),
        size: data.len() as u64,
        url: url(1, data),
    }
}

fn ie(n: u64, data: &str, name: &str, folder: &str) -> IndexEntry {
    IndexEntry::new(
        format!("{}_{name}", sha(data)),
        name,
        Some(url(n, data)),
        Some(data.len() as u64),
        None,
        folder,
    )
}

fn index(entries: Vec<IndexEntry>) -> CloudIndex {
    let mut index = CloudIndex::empty();
    index.entries = entries;
    index
}

fn local(path: &str, data: &str) -> LocalFile {
    LocalFile {
        path: path.to_owned(),
        size: data.len() as u64,
        sha1: sha(data),
    }
}

fn manifest(entries: Vec<ManifestEntry>) -> Manifest {
    Manifest { entries }
}

#[test]
fn output_is_sorted_and_independent_of_insertion_order() {
    let a = me(Some("a.png"), "G/a.png", "a");
    let b = me(Some("B.png"), "G/B.png", "b");
    let c = me(Some("c.png"), "g/c.png", "c");
    let d = me(None, "G/a.png", "a2");
    let one = manifest(vec![c.clone(), b.clone(), a.clone(), d.clone()]).to_json();
    let two = manifest(vec![d.clone(), a.clone(), c.clone(), b.clone()]).to_json();
    assert_eq!(one, two);
    let parsed = Manifest::parse(&one).unwrap();
    assert_eq!(parsed.entries, [d, a, b, c]);
}

#[test]
fn exact_duplicates_are_dropped() {
    let a = me(Some("a.png"), "G/a.png", "a");
    let json = manifest(vec![a.clone(), a.clone()]).to_json();
    assert_eq!(Manifest::parse(&json).unwrap().entries, [a]);
}

#[test]
fn json_layout() {
    let m = manifest(vec![me(None, "x.png", "x"), me(Some("p"), "y.png", "y")]);
    let json = m.to_json();
    assert!(json.ends_with("}\n"));
    assert!(
        json.starts_with("{\n  \"version\": 1,\n  \"entries\": [\n    {\n      \"cloud_path\"")
    );
    let first = json.find("\"cloud_path\": \"x.png\"").unwrap();
    assert!(!json[..first].contains("\"path\""));
    let p = json.find("\"path\": \"p\"").unwrap();
    let order: Vec<usize> = [
        "\"path\"",
        "\"cloud_path\": \"y",
        "\"sha1\"",
        "\"size\"",
        "\"url\"",
    ]
    .iter()
    .map(|k| p + json[p..].find(k).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn save_and_load_round_trip_with_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(Manifest::FILE_NAME);
    let m = manifest(vec![
        me(Some("b.png"), "G/b.png", "b"),
        me(None, "G/a", "a"),
    ]);
    m.save(&file).unwrap();
    let first = fs::read(&file).unwrap();
    let loaded = Manifest::load(&file).unwrap();
    assert_eq!(loaded.entries.len(), 2);
    loaded.save(&file).unwrap();
    assert_eq!(fs::read(&file).unwrap(), first);
    m.save(&file).unwrap();
    assert_eq!(fs::read(&file).unwrap(), first);
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, [Manifest::FILE_NAME]);
}

#[test]
fn missing_file_is_an_empty_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let m = Manifest::load(&dir.path().join("nope.json")).unwrap();
    assert!(m.entries.is_empty());
}

#[test]
fn load_reports_other_io_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        Manifest::load(dir.path()),
        Err(ManifestError::Io { .. })
    ));
}

#[test]
fn parse_errors() {
    assert!(matches!(
        Manifest::parse("{"),
        Err(ManifestError::Invalid(_))
    ));
    let err = Manifest::parse(r#"{"version":2,"entries":[]}"#).unwrap_err();
    assert_eq!(err, ManifestError::UnsupportedVersion("2".into()));
    assert!(err.to_string().contains('2'));
    assert!(matches!(
        Manifest::parse(r#"{"entries":[]}"#),
        Err(ManifestError::Invalid(_))
    ));
    let bad = r#"{"version":1,"entries":[{"cloud_path":"a","sha1":"ABC","size":1,"url":"u"}]}"#;
    assert!(matches!(
        Manifest::parse(bad),
        Err(ManifestError::BadSha1 { .. })
    ));
    let unknown = format!(
        r#"{{"version":1,"entries":[{{"cloud_path":"a","sha1":"{}","size":1,"url":"u","extra":1}}]}}"#,
        sha("a")
    );
    assert!(matches!(
        Manifest::parse(&unknown),
        Err(ManifestError::Invalid(_))
    ));
    assert!(matches!(
        Manifest::parse(r#"{"version":1,"entries":[],"extra":1}"#),
        Err(ManifestError::Invalid(_))
    ));
}

#[test]
fn parse_uppercases_sha1() {
    let text = format!(
        r#"{{"version":1,"entries":[{{"cloud_path":"a","sha1":"{}","size":1,"url":"u"}}]}}"#,
        sha("a").to_lowercase()
    );
    assert_eq!(Manifest::parse(&text).unwrap().entries[0].sha1, sha("a"));
}

#[test]
fn upsert_by_path() {
    let mut m = Manifest::default();
    m.upsert(me(Some("a.png"), "G/a.png", "v1"));
    m.upsert(me(Some("b.png"), "G/b.png", "b"));
    m.upsert(me(None, "G/a.png", "v1"));
    m.upsert(me(Some("a.png"), "G/a.png", "v2"));
    assert_eq!(m.entries.len(), 3);
    assert_eq!(m.entries[0].sha1, sha("v2"));
}

#[test]
fn upsert_without_path_matches_cloud_path_and_sha1() {
    let mut m = Manifest::default();
    let mut lower = me(None, "G/a.png", "v1");
    lower.sha1 = lower.sha1.to_lowercase();
    m.upsert(lower);
    let mut newer = me(None, "G/a.png", "v1");
    newer.url = url(9, "v1");
    m.upsert(newer.clone());
    assert_eq!(m.entries, [newer]);
    m.upsert(me(None, "G/a.png", "v2"));
    m.upsert(me(None, "G/other.png", "v1"));
    // A pathed entry is never replaced by a path-less one.
    m.upsert(me(Some("x"), "G/x.png", "x"));
    m.upsert(me(None, "G/x.png", "x"));
    assert_eq!(m.entries.len(), 5);
}

#[test]
fn from_index_cases() {
    let e = ie(7, "abc", "a.png", r"My Game\Boards\");
    let m = ManifestEntry::from_index(&e, Some("a.png".into())).unwrap();
    assert_eq!(m.cloud_path, "My Game/Boards/a.png");
    assert_eq!(m.sha1, sha("abc"));
    assert_eq!(m.size, 3);
    assert_eq!(m.url, url(7, "abc"));
    assert_eq!(m.path.as_deref(), Some("a.png"));
    let root = ie(7, "abc", "a.png", "");
    assert_eq!(
        ManifestEntry::from_index(&root, None).unwrap().cloud_path,
        "a.png"
    );
    let mut lower = ie(7, "abc", "a.png", "");
    lower.key = lower.key.to_lowercase();
    assert_eq!(
        ManifestEntry::from_index(&lower, None).unwrap().sha1,
        sha("abc")
    );

    let mut no_url = ie(7, "abc", "a.png", "");
    no_url.url = None;
    assert!(ManifestEntry::from_index(&no_url, None).is_none());
    let mut no_size = ie(7, "abc", "a.png", "");
    no_size.size = None;
    assert!(ManifestEntry::from_index(&no_size, None).is_none());
    let mut bad_key = ie(7, "abc", "a.png", "");
    bad_key.key = "XY_a.png".into();
    assert!(ManifestEntry::from_index(&bad_key, None).is_none());
}

#[test]
fn tsv_export_format() {
    let m = manifest(vec![
        me(Some("b.png"), "G/b.png", "b"),
        me(None, "G/a.png", "a"),
        me(Some("C/d.png"), "G/C/d.png", "d"),
    ]);
    let expected = format!(
        "G/a.png\t{}\nb.png\t{}\nC/d.png\t{}\n",
        url(1, "a"),
        url(1, "b"),
        url(1, "d")
    );
    // Sorted by cloud path: G/a.png, G/b.png, G/C/d.png.
    assert_eq!(m.to_tsv(), expected);
    assert_eq!(Manifest::default().to_tsv(), "");
}

#[test]
fn tsv_round_trip() {
    let idx = index(vec![
        ie(1, "a", "a.png", "G"),
        ie(2, "b", "b.png", r"G\Sub"),
        ie(3, "c", "c.png", ""),
    ]);
    let entries: Vec<_> = [("a.png", 0), ("Sub/b.png", 1), ("c.png", 2)]
        .iter()
        .map(|(p, i)| ManifestEntry::from_index(&idx.entries[*i], Some((*p).into())).unwrap())
        .collect();
    let m = manifest(entries);
    let imported = Manifest::import_tsv(&m.to_tsv(), &idx).unwrap();
    assert_eq!(manifest(imported).to_json(), m.to_json());
}

fn problems(text: &str, idx: &CloudIndex) -> Vec<(usize, String)> {
    Manifest::import_tsv(text, idx)
        .unwrap_err()
        .problems
        .into_iter()
        .map(|p| (p.line, p.message))
        .collect()
}

#[test]
fn tsv_malformed_lines_are_reported_with_line_numbers() {
    let idx = index(vec![ie(1, "a", "a.png", "")]);
    let good = url(1, "a");
    let cases: Vec<(String, &str)> = vec![
        ("a.png".into(), "no tab"),
        (format!("a.png\t{good}\textra"), "exactly one tab"),
        (format!("\t{good}"), "filename is empty"),
        ("a.png\t".into(), "URL is empty"),
        (
            "a.png\thttp://example.com/x".into(),
            "not a valid cloud URL",
        ),
        (url(2, "zzz"), "no tab"),
        (
            format!("a.png\t{}", url(2, "zzz")),
            "not found in the cloud index",
        ),
        (format!("../a.png\t{good}"), ".."),
        (format!("sub/../a.png\t{good}"), ".."),
        (format!("/a.png\t{good}"), "absolute"),
        (format!("C:/a.png\t{good}"), "absolute"),
        (format!("sub\\a.png\t{good}"), "backslash"),
        (format!("a\u{1}.png\t{good}"), "control"),
    ];
    for (line, want) in cases {
        let got = problems(&format!("# comment\n\n{line}\n"), &idx);
        assert_eq!(got.len(), 1, "{line:?}");
        assert_eq!(got[0].0, 3, "{line:?}");
        assert!(got[0].1.contains(want), "{line:?}: {}", got[0].1);
    }
}

#[test]
fn tsv_duplicate_filename_and_missing_size() {
    let mut no_size = ie(2, "b", "b.png", "");
    no_size.size = None;
    let idx = index(vec![ie(1, "a", "a.png", ""), no_size]);
    let text = format!(
        "a.png\t{}\nA.PNG\t{}\nb.png\t{}\n",
        url(1, "a"),
        url(1, "a"),
        url(2, "b")
    );
    let got = problems(&text, &idx);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, 2);
    assert!(got[0].1.contains("duplicate"));
    assert_eq!(got[1].0, 3);
    assert!(got[1].1.contains("no size"));
}

#[test]
fn tsv_accepts_crlf_blank_and_comment_lines() {
    let idx = index(vec![ie(1, "a", "a.png", "G"), ie(2, "b", "b.png", "G")]);
    let text = format!(
        "# header\r\n\r\na.png\t{}\r\n   \r\n#b.png\t{}\r\nb.png\t{}",
        url(1, "a"),
        url(2, "b"),
        url(2, "b")
    );
    let got = Manifest::import_tsv(&text, &idx).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].path.as_deref(), Some("a.png"));
    assert_eq!(got[0].cloud_path, "G/a.png");
    assert_eq!(got[1].url, url(2, "b"));
    assert!(Manifest::import_tsv("", &idx).unwrap().is_empty());
}

#[test]
fn tsv_reports_all_problems_together() {
    let idx = index(vec![ie(1, "a", "a.png", "")]);
    let text = format!(
        "bad\n{}\t{}\nok.png\t{}\n../x\t{}\n",
        "a.png",
        "nonsense",
        url(1, "a"),
        url(1, "a")
    );
    let err = Manifest::import_tsv(&text, &idx).unwrap_err();
    let lines: Vec<usize> = err.problems.iter().map(|p| p.line).collect();
    assert_eq!(lines, [1, 2, 4]);
    let shown = err.to_string();
    assert_eq!(shown.lines().count(), 3);
    assert!(shown.starts_with("line 1: "));
}

#[test]
fn tsv_matches_url_by_handle_and_sha1_not_spelling() {
    let old = IndexEntry::new(
        format!("{}_a.png", sha("a")),
        "a.png",
        Some(format!(
            "http://cloud-3.steamusercontent.com/ugc/5/{}",
            sha("a")
        )),
        Some(1),
        None,
        "",
    );
    let idx = index(vec![old]);
    let text = format!("a.png\t{}\n", url(5, "a"));
    let got = Manifest::import_tsv(&text, &idx).unwrap();
    assert!(got[0].url.starts_with("http://cloud-3"));
}

#[test]
fn refresh_adds_updates_and_drops() {
    let idx = index(vec![
        ie(1, "a2", "a.png", "G"),
        ie(2, "b", "B.png", r"g\Sub"),
        ie(3, "z", "z.png", "Other"),
    ]);
    let mut m = manifest(vec![
        me(Some("a.png"), "G/a.png", "a1"),
        me(Some("gone.png"), "G/gone.png", "gone"),
        me(None, "Keep/k.png", "k"),
    ]);
    m.refresh_for_local(
        &[
            local("a.png", "a2"),
            local("sub/b.png", "b"),
            local("unmatched.png", "u"),
        ],
        "/G/",
        &idx,
    );
    m.entries.sort_by(ManifestEntry::order);
    assert_eq!(m.entries.len(), 3);
    assert_eq!(m.entries[0].cloud_path, "G/a.png");
    assert_eq!(m.entries[0].sha1, sha("a2"));
    assert_eq!(m.entries[1].path.as_deref(), Some("sub/b.png"));
    assert_eq!(m.entries[1].cloud_path, "g/Sub/B.png");
    assert_eq!(m.entries[2].cloud_path, "Keep/k.png");
    assert_eq!(m.entries[2].path, None);
}

#[test]
fn refresh_prefers_the_entry_with_the_files_sha1() {
    let idx = index(vec![ie(1, "old", "a.png", ""), ie(2, "new", "a.png", "")]);
    let mut m = Manifest::default();
    m.refresh_for_local(&[local("a.png", "new")], "", &idx);
    assert_eq!(m.entries.len(), 1);
    assert_eq!(m.entries[0].url, url(2, "new"));
}
