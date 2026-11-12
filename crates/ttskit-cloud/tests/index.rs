use bson::{Bson, Document, doc};
use ttskit_cloud::{
    CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, CloudError, CloudIndex, IndexEntry, IndexError,
    MemoryCloud, Op, UgcHandle, UgcUrl, is_game_internal, normalize_folder,
};

const SHA: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

fn url(handle: u64) -> String {
    format!("https://steamusercontent-a.akamaihd.net/ugc/{handle}/{SHA}/")
}

fn key(name: &str) -> String {
    format!("{SHA}_{name}")
}

fn info(entries: Vec<(String, Bson)>) -> Vec<u8> {
    entries.into_iter().collect::<Document>().to_vec().unwrap()
}

fn full_entry() -> Document {
    doc! {
        "Name": "a.png",
        "URL": url(42),
        "Size": 1234_i64,
        "Date": "5/1/2024 10:00 AM",
        "Folder": "x/y",
    }
}

fn folders(list: &[&str]) -> Vec<u8> {
    let d: Document = list
        .iter()
        .enumerate()
        .map(|(i, f)| (i.to_string(), Bson::String((*f).into())))
        .collect();
    d.to_vec().unwrap()
}

fn decode_info(entries: Vec<(String, Bson)>) -> Result<CloudIndex, IndexError> {
    CloudIndex::decode(Some(&info(entries)), None)
}

fn size_doc(size: Bson) -> Vec<(String, Bson)> {
    vec![(key("a"), Bson::Document(doc! { "Name": "a", "Size": size }))]
}

#[test]
fn parses_entries_and_folders() {
    let bytes = info(vec![
        (key("a.png"), Bson::Document(full_entry())),
        (key("b"), Bson::Document(doc! { "Name": "b", "Folder": "" })),
    ]);
    let idx = CloudIndex::decode(Some(&bytes), Some(&folders(&["x", "x/y"]))).unwrap();
    assert_eq!(idx.entries.len(), 2);
    let e = &idx.entries[0];
    assert_eq!(e.key, key("a.png"));
    assert_eq!(e.sha1(), SHA);
    assert_eq!(e.name, "a.png");
    assert_eq!(e.url.as_deref(), Some(url(42).as_str()));
    assert_eq!(e.size, Some(1234));
    assert_eq!(e.date.as_deref(), Some("5/1/2024 10:00 AM"));
    assert_eq!(e.folder, "x/y");
    assert_eq!(idx.entries[1].folder, "");
    assert_eq!(idx.folders, ["x", "x/y"]);
}

#[test]
fn all_folders_splits_on_backslash_as_the_game_writes() {
    let bytes = info(vec![(
        key("a"),
        Bson::Document(doc! { "Folder": "Game\\dice" }),
    )]);
    let idx = CloudIndex::decode(
        Some(&bytes),
        Some(&folders(&["Game", "Game\\token", "x/y\\z"])),
    )
    .unwrap();
    assert_eq!(
        idx.all_folders(),
        ["Game", "Game/dice", "Game/token", "x", "x/y", "x/y/z"]
    );
    // The stored strings are kept as the game wrote them.
    assert_eq!(idx.entries[0].folder, "Game\\dice");
    assert_eq!(idx.folders[1], "Game\\token");
}

#[test]
fn normalize_folder_accepts_both_separators() {
    assert_eq!(normalize_folder("\\a\\\\b/c/"), "a/b/c");
    assert_eq!(normalize_folder("\\/"), "");
    assert_eq!(normalize_folder(""), "");
}

#[test]
fn all_folders_adds_ancestors_and_entry_folders() {
    let bytes = info(vec![
        (key("a"), Bson::Document(doc! { "Folder": "m/n/o" })),
        (key("b"), Bson::Document(doc! { "Folder": "" })),
        (key("c"), Bson::Document(doc! { "Folder": "/B//c/" })),
    ]);
    let idx = CloudIndex::decode(Some(&bytes), Some(&folders(&["z/y", "b", "", "m/n"]))).unwrap();
    assert_eq!(
        idx.all_folders(),
        ["B", "b", "B/c", "m", "m/n", "m/n/o", "z", "z/y"]
    );
}

#[test]
fn missing_files_are_empty() {
    let idx = CloudIndex::decode(None, None).unwrap();
    assert!(idx.entries.is_empty() && idx.folders.is_empty());
    assert!(idx.all_folders().is_empty());
}

#[test]
fn load_with_both_one_and_no_files() {
    let info_bytes = info(vec![(key("a.png"), Bson::Document(full_entry()))]);
    let both = MemoryCloud::new()
        .with_file(CLOUD_INFO_FILE, info_bytes.clone())
        .with_file(CLOUD_FOLDER_FILE, folders(&["x"]));
    let idx = CloudIndex::load(&both).unwrap();
    assert_eq!((idx.entries.len(), idx.folders.len()), (1, 1));

    let only_info = MemoryCloud::new().with_file(CLOUD_INFO_FILE, info_bytes);
    let idx = CloudIndex::load(&only_info).unwrap();
    assert_eq!((idx.entries.len(), idx.folders.len()), (1, 0));

    let only_folders = MemoryCloud::new().with_file(CLOUD_FOLDER_FILE, folders(&["x"]));
    let idx = CloudIndex::load(&only_folders).unwrap();
    assert_eq!((idx.entries.len(), idx.folders.len()), (0, 1));

    let idx = CloudIndex::load(&MemoryCloud::new()).unwrap();
    assert!(idx.entries.is_empty() && idx.folders.is_empty());
}

#[test]
fn load_propagates_other_read_errors() {
    let cloud = MemoryCloud::new();
    cloud.fail_once(
        Op::Read,
        Some(CLOUD_INFO_FILE),
        CloudError::Backend("boom".into()),
    );
    let err = CloudIndex::load(&cloud).unwrap_err();
    assert_eq!(err, IndexError::Cloud(CloudError::Backend("boom".into())));
}

#[test]
fn invalid_bson_names_the_file() {
    let err = CloudIndex::decode(Some(&[1, 2, 3]), None).unwrap_err();
    assert!(matches!(
        err,
        IndexError::Bson {
            file: "CloudInfo.bson",
            ..
        }
    ));
    assert!(err.to_string().contains("CloudInfo.bson"));
    let err = CloudIndex::decode(None, Some(&[1, 2, 3])).unwrap_err();
    assert!(err.to_string().contains("CloudFolder.bson"));
}

#[test]
fn size_accepts_int32_int64_and_whole_double() {
    for size in [Bson::Int32(7), Bson::Int64(7), Bson::Double(7.0)] {
        let idx = decode_info(size_doc(size)).unwrap();
        assert_eq!(idx.entries[0].size, Some(7));
    }
}

#[test]
fn size_rejects_negative_and_fractional() {
    for size in [
        Bson::Int32(-1),
        Bson::Int64(-1),
        Bson::Double(-1.0),
        Bson::Double(1.5),
        Bson::Double(f64::NAN),
        Bson::String("7".into()),
    ] {
        let err = decode_info(size_doc(size)).unwrap_err();
        assert!(
            matches!(&err, IndexError::FieldType { field: "Size", .. }),
            "{err:?}"
        );
    }
}

#[test]
fn wrong_type_field_names_key_and_field() {
    let err = decode_info(vec![(key("a"), Bson::Document(doc! { "Name": 5_i32 }))]).unwrap_err();
    assert!(matches!(&err, IndexError::FieldType { key: k, field: "Name", .. } if *k == key("a")));
    let msg = err.to_string();
    assert!(msg.contains("Name") && msg.contains(&key("a")));
}

#[test]
fn bad_keys_are_rejected() {
    for k in ["nounderscore", "_name"] {
        let err = decode_info(vec![(k.into(), Bson::Document(doc! {}))]).unwrap_err();
        assert_eq!(err, IndexError::BadKey { key: k.into() });
    }
}

#[test]
fn missing_fields_use_defaults() {
    let idx = decode_info(vec![(key("my_file.png"), Bson::Document(doc! {}))]).unwrap();
    let e = &idx.entries[0];
    assert_eq!(e.name, "my_file.png");
    assert_eq!(e.folder, "");
    assert_eq!((&e.url, e.size, &e.date), (&None, None, &None));
}

#[test]
fn non_document_entry_is_rejected() {
    let err = decode_info(vec![(key("a"), Bson::Int32(1))]).unwrap_err();
    assert_eq!(err, IndexError::NotADocument { key: key("a") });
}

#[test]
fn non_string_folder_is_rejected() {
    let d = doc! { "0": "a", "1": 5_i32 };
    let err = CloudIndex::decode(None, Some(&d.to_vec().unwrap())).unwrap_err();
    assert_eq!(err, IndexError::FolderType { index: 1 });
    assert!(err.to_string().contains("CloudFolder.bson"));
}

#[test]
fn unmodified_info_round_trips_byte_identical() {
    let bytes = info(vec![
        (
            key("a.png"),
            Bson::Document(doc! {
                "Name": "a.png",
                "Unknown": [1_i32, 2_i32],
                "URL": url(1),
                "Size": 12_i32,
                "Date": "d",
                "Folder": "x",
            }),
        ),
        (key("b"), Bson::Document(doc! { "Size": 3.0_f64 })),
        (
            key("c"),
            Bson::Document(doc! { "Folder": "q", "Name": "c" }),
        ),
        (
            key("d"),
            Bson::Document(doc! { "Size": 5_i64, "URL": Bson::Null }),
        ),
    ]);
    let idx = CloudIndex::decode(Some(&bytes), None).unwrap();
    assert_eq!(idx.encode_info().unwrap(), bytes);
}

#[test]
fn unmodified_folders_round_trip_byte_identical() {
    let bytes = folders(&["a", "a/b"]);
    let idx = CloudIndex::decode(None, Some(&bytes)).unwrap();
    assert_eq!(idx.encode_folders().unwrap(), bytes);
    // Odd keys survive too while the list is unchanged.
    let odd = doc! { "5": "a", "x": "b" }.to_vec().unwrap();
    let idx = CloudIndex::decode(None, Some(&odd)).unwrap();
    assert_eq!(idx.encode_folders().unwrap(), odd);
}

#[test]
fn changed_folders_are_rebuilt() {
    let mut idx = CloudIndex::decode(None, Some(&folders(&["a"]))).unwrap();
    idx.folders.push("b".into());
    assert_eq!(idx.encode_folders().unwrap(), folders(&["a", "b"]));
    let empty = CloudIndex::decode(None, None).unwrap();
    assert_eq!(empty.encode_folders().unwrap(), folders(&[]));
}

fn encoded_entry(idx: &CloudIndex, k: &str) -> Document {
    let root = Document::from_reader(&idx.encode_info().unwrap()[..]).unwrap();
    root.get_document(k).unwrap().clone()
}

#[test]
fn editing_known_field_keeps_unknown_field_in_place() {
    let d = doc! { "Name": "a", "Extra": true, "Folder": "x", "Zed": 1_i32 };
    let mut idx = decode_info(vec![(key("a"), Bson::Document(d))]).unwrap();
    idx.entries[0].folder = "y".into();
    idx.entries[0].size = Some(9);
    let out = encoded_entry(&idx, &key("a"));
    let keys: Vec<&String> = out.keys().collect();
    assert_eq!(keys, ["Name", "Extra", "Folder", "Zed", "Size"]);
    assert_eq!(out.get_str("Folder").unwrap(), "y");
    assert_eq!(out.get_i64("Size").unwrap(), 9);
    assert!(out.get_bool("Extra").unwrap());
}

#[test]
fn clearing_url_removes_the_field() {
    let mut idx = decode_info(vec![(key("a.png"), Bson::Document(full_entry()))]).unwrap();
    idx.entries[0].url = None;
    let out = encoded_entry(&idx, &key("a.png"));
    assert!(!out.contains_key("URL"));
    assert_eq!(
        out.keys().collect::<Vec<_>>(),
        ["Name", "Size", "Date", "Folder"]
    );
}

#[test]
fn size_keeps_raw_type_when_edited() {
    let mut idx = decode_info(size_doc(Bson::Int32(7))).unwrap();
    idx.entries[0].size = Some(8);
    assert_eq!(
        encoded_entry(&idx, &key("a")).get("Size"),
        Some(&Bson::Int32(8))
    );
    idx.entries[0].size = Some(u64::from(u32::MAX));
    assert_eq!(
        encoded_entry(&idx, &key("a")).get("Size"),
        Some(&Bson::Int64(i64::from(u32::MAX)))
    );
    let mut idx = decode_info(size_doc(Bson::Double(7.0))).unwrap();
    idx.entries[0].size = Some(8);
    assert_eq!(
        encoded_entry(&idx, &key("a")).get("Size"),
        Some(&Bson::Double(8.0))
    );
}

#[test]
fn new_entry_fields_are_written_in_order() {
    let mut idx = CloudIndex::decode(None, None).unwrap();
    idx.entries.push(IndexEntry::new(
        key("n.png"),
        "n.png",
        Some(url(1)),
        Some(5),
        Some("d".into()),
        "f",
    ));
    let out = encoded_entry(&idx, &key("n.png"));
    assert_eq!(
        out.keys().collect::<Vec<_>>(),
        ["URL", "Folder", "Name", "Size", "Date"]
    );
    // And it decodes back to the same values.
    let back = CloudIndex::decode(Some(&idx.encode_info().unwrap()), None).unwrap();
    assert_eq!(back.entries[0].size, Some(5));
}

#[test]
fn ugc_url_parses_both_hosts() {
    let expect = UgcUrl {
        handle: UgcHandle(42),
        sha1: SHA.into(),
    };
    for u in [
        format!("https://steamusercontent-a.akamaihd.net/ugc/42/{SHA}/"),
        format!("https://steamusercontent-a.akamaihd.net/ugc/42/{SHA}"),
        format!("http://cloud-3.steamusercontent.com/ugc/42/{SHA}/"),
        format!("http://cloud-3.steamusercontent.com/ugc/42/{SHA}"),
    ] {
        assert_eq!(UgcUrl::parse(&u), Some(expect.clone()), "{u}");
    }
}

#[test]
fn ugc_url_rejects_invalid_input() {
    for u in [
        format!("https://example.com/ugc/42/{SHA}/"),
        format!("https://steamusercontent-a.akamaihd.net/ugc/4x2/{SHA}/"),
        format!("https://steamusercontent-a.akamaihd.net/ugc/99999999999999999999/{SHA}/"),
        "https://steamusercontent-a.akamaihd.net/ugc/42//".to_string(),
        "https://steamusercontent-a.akamaihd.net/ugc/42/".to_string(),
        "https://steamusercontent-a.akamaihd.net/ugc//ABC/".to_string(),
        format!("https://steamusercontent-a.akamaihd.net/ugc/42/{SHA}/extra"),
        format!("https://steamusercontent-a.akamaihd.net/ugc/42/{SHA}//"),
        "https://steamusercontent-a.akamaihd.net/ugc/42/AB-C/".to_string(),
        String::new(),
    ] {
        assert_eq!(UgcUrl::parse(&u), None, "{u}");
    }
}

#[test]
fn ugc_url_display_round_trips() {
    let u = UgcUrl::parse(&format!("http://cloud-3.steamusercontent.com/ugc/42/{SHA}")).unwrap();
    assert_eq!(u.to_string(), url(42));
    assert_eq!(UgcUrl::parse(&u.to_string()), Some(u));
}

#[test]
fn game_internal_files_match_case_insensitively() {
    assert!(is_game_internal("cloudinfo.BSON"));
    assert!(is_game_internal("CloudFolder.bson"));
    assert!(is_game_internal("workshopupload"));
    assert!(is_game_internal("WORKSHOPIMAGEUPLOAD.PNG"));
    assert!(!is_game_internal("other.png"));
}
