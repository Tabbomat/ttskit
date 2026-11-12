//! Tests for the asset URL walker.

use serde_json::json;
use ttskit_core::Save;
use ttskit_core::save::assets::{AssetUrl, HostClass};

fn save(value: serde_json::Value) -> Save {
    Save::from_value(value).unwrap()
}

fn find<'a>(urls: &'a [AssetUrl], url: &str) -> &'a AssetUrl {
    urls.iter()
        .find(|u| u.url == url)
        .unwrap_or_else(|| panic!("missing {url}"))
}

fn summary(urls: &[AssetUrl]) -> Vec<(&str, Vec<&str>, usize)> {
    urls.iter()
        .map(|u| {
            (
                u.url.as_str(),
                u.labels.iter().map(String::as_str).collect(),
                u.count,
            )
        })
        .collect()
}

#[test]
fn fixture_expected_list() {
    let s = Save::from_slice(&ttskit_testutil::read_fixture("custom_assets.json")).unwrap();
    let urls = s.asset_urls();
    let steam_face = "https://steamusercontent-a.akamaihd.net/ugc/0000000000000000000/0000000000000000000000000000000000000000/";
    let steam_back = "http://cloud-3.steamusercontent.com/ugc/000000000000000000/00000000000000000000000000000000000000AA/";
    let expected: Vec<(&str, Vec<&str>, usize)> = vec![
        (
            "http://cloud-3.steamusercontent.com/ugc/000000000000000000/00000000000000000000000000000000000000AA/",
            vec!["BackURL"],
            3,
        ),
        ("https://example.com/audio/loop.ogg", vec!["Item1"], 1),
        (
            "https://example.com/audio/song.mp3",
            vec!["CurrentAudioURL", "Item1"],
            2,
        ),
        (
            "https://example.com/bundle.unity3d",
            vec!["AssetbundleURL"],
            1,
        ),
        ("https://example.com/image.png", vec!["ImageURL"], 1),
        (
            "https://example.com/image_back.png",
            vec!["ImageSecondaryURL"],
            1,
        ),
        ("https://example.com/lut.png", vec!["LutURL"], 1),
        ("https://example.com/model.obj", vec!["MeshURL"], 1),
        ("https://example.com/model_c.obj", vec!["ColliderURL"], 1),
        ("https://example.com/model_d.png", vec!["DiffuseURL"], 1),
        ("https://example.com/model_n.png", vec!["NormalURL"], 1),
        ("https://example.com/rules.pdf", vec!["PDFUrl"], 1),
        ("https://example.com/sky.jpg", vec!["SkyURL"], 1),
        ("https://example.com/table.jpg", vec!["TableURL"], 1),
        ("https://example.com/ui/font.ttf", vec!["CustomUIAssets"], 1),
        ("https://i.imgur.com/AAAAAAA.png", vec!["CustomUIAssets"], 1),
        (
            "https://steamusercontent-a.akamaihd.net/ugc/0000000000000000000/0000000000000000000000000000000000000000/",
            vec!["FaceURL"],
            3,
        ),
    ];
    assert_eq!(summary(&urls), expected);
    assert_eq!(
        find(&urls, "https://i.imgur.com/AAAAAAA.png").host_class(),
        HostClass::Imgur
    );
    assert_eq!(
        find(&urls, steam_back).host_class(),
        HostClass::SteamUserContent
    );
    assert_eq!(
        find(&urls, steam_face).host_class(),
        HostClass::SteamUserContent
    );
    assert_eq!(
        find(&urls, "https://example.com/lut.png").host_class(),
        HostClass::Other
    );
}

#[test]
fn fixture_still_round_trips() {
    let input = ttskit_testutil::read_fixture("custom_assets.json");
    assert_eq!(Save::from_slice(&input).unwrap().to_vec(), input);
}

#[test]
fn sorted_by_byte_order_and_deterministic() {
    let s = save(json!({
        "b": "https://b.example.com/",
        "a": "https://B.example.com/",
        "c": "https://a.example.com/"
    }));
    let urls = s.asset_urls();
    let order: Vec<&str> = urls.iter().map(|u| u.url.as_str()).collect();
    assert_eq!(
        order,
        [
            "https://B.example.com/",
            "https://a.example.com/",
            "https://b.example.com/"
        ]
    );
    assert_eq!(urls, s.asset_urls());
}

#[test]
fn duplicates_aggregate_with_labels() {
    let s = save(json!({
        "A": "https://example.com/x.png",
        "O": { "B": "https://example.com/x.png", "C": ["https://example.com/x.png"] },
        "A2": "https://example.com/x.png"
    }));
    let urls = s.asset_urls();
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].count, 4);
    assert_eq!(urls[0].labels, ["A", "A2", "B", "C"]);
}

#[test]
fn same_label_is_listed_once() {
    let s = save(json!({
        "L": [{ "ImageURL": "https://example.com/x" }, { "ImageURL": "https://example.com/x" }]
    }));
    let urls = s.asset_urls();
    assert_eq!(urls[0].labels, ["ImageURL"]);
    assert_eq!(urls[0].count, 2);
}

#[test]
fn nested_objects_and_arrays_use_nearest_key() {
    let s = save(json!({
        "ObjectStates": [{
            "ContainedObjects": [{
                "CustomImage": { "ImageURL": "https://example.com/deep.png" }
            }],
            "Tags": ["https://example.com/in-array", ["https://example.com/nested-array"]]
        }]
    }));
    let urls = s.asset_urls();
    assert_eq!(
        summary(&urls),
        [
            ("https://example.com/deep.png", vec!["ImageURL"], 1),
            ("https://example.com/in-array", vec!["Tags"], 1),
            ("https://example.com/nested-array", vec!["Tags"], 1),
        ]
    );
}

#[test]
fn custom_ui_assets_label_only_for_url_key_of_elements() {
    let s = save(json!({
        "CustomUIAssets": [
            { "Type": 0, "Name": "a", "URL": "https://example.com/a.png" },
            { "Other": "https://example.com/o.png" }
        ],
        "Elsewhere": [{ "URL": "https://example.com/e.png" }],
        "URL": "https://example.com/top.png"
    }));
    let urls = s.asset_urls();
    assert_eq!(
        summary(&urls),
        [
            ("https://example.com/a.png", vec!["CustomUIAssets"], 1),
            ("https://example.com/e.png", vec!["URL"], 1),
            ("https://example.com/o.png", vec!["Other"], 1),
            ("https://example.com/top.png", vec!["URL"], 1),
        ]
    );
}

#[test]
fn non_urls_are_ignored() {
    let s = save(json!({
        "A": "ftp://example.com/x",
        "B": "see https://example.com/x",
        "C": " https://example.com/x",
        "D": "",
        "E": 5,
        "F": null,
        "G": true,
        "H": "http:/example.com",
        "I": "héllo",
        "https://example.com/key": "value"
    }));
    assert!(s.asset_urls().is_empty());
}

#[test]
fn scheme_is_case_insensitive() {
    let s = save(json!({ "A": "HTTPS://Example.com/x", "B": "Http://example.com/y" }));
    assert_eq!(s.asset_urls().len(), 2);
}

#[test]
fn urls_keep_exact_text() {
    let s = save(json!({ "A": "https://example.com/x y?q=1#f" }));
    assert_eq!(s.asset_urls()[0].url, "https://example.com/x y?q=1#f");
}

fn class(url: &str) -> HostClass {
    save(json!({ "A": url })).asset_urls()[0].host_class()
}

#[test]
fn host_classification() {
    assert_eq!(class("https://imgur.com/a"), HostClass::Imgur);
    assert_eq!(class("https://i.imgur.com/a.png"), HostClass::Imgur);
    assert_eq!(class("HTTPS://I.IMGUR.COM/a.png"), HostClass::Imgur);
    assert_eq!(class("https://notimgur.com/a.png"), HostClass::Other);
    assert_eq!(class("https://imgur.com.evil.example/a"), HostClass::Other);
    assert_eq!(class("https://example.com/imgur.com/a"), HostClass::Other);
    assert_eq!(
        class("https://steamusercontent-a.akamaihd.net/ugc/0/0/"),
        HostClass::SteamUserContent
    );
    assert_eq!(
        class("http://cloud-3.steamusercontent.com/ugc/0/0/"),
        HostClass::SteamUserContent
    );
    assert_eq!(
        class("https://example.com/steamusercontent"),
        HostClass::Other
    );
}

#[test]
fn host_classification_ignores_userinfo_and_port() {
    assert_eq!(
        class("https://user:pw@i.imgur.com:443/a.png"),
        HostClass::Imgur
    );
    assert_eq!(class("https://imgur.com:8080"), HostClass::Imgur);
    assert_eq!(class("https://imgur.com?x=1"), HostClass::Imgur);
    assert_eq!(class("https://imgur.com#frag"), HostClass::Imgur);
    assert_eq!(class("https://imgur.com@example.com/a"), HostClass::Other);
    assert_eq!(
        class("https://example.com@cloud-3.steamusercontent.com/"),
        HostClass::SteamUserContent
    );
}

#[test]
fn host_class_names() {
    assert_eq!(HostClass::Imgur.as_str(), "imgur");
    assert_eq!(HostClass::SteamUserContent.as_str(), "steamusercontent");
    assert_eq!(HostClass::Other.as_str(), "other");
    assert!(HostClass::Imgur < HostClass::SteamUserContent);
    assert!(HostClass::SteamUserContent < HostClass::Other);
}
