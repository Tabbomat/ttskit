//! `save pack` / `save unpack --assets` with `asset:{PATH}` placeholders.
//! Local files only.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const URL: &str = "https://example.invalid/ugc/1/AAAA/";
const CONFIG: &str = "[cloud]\nfolder = \"Game\"\n\n[local]\ndir = \"src\"\n";

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn manifest_entry(path: &str, sha1: &str, url: &str) -> String {
    format!(
        "{{\"path\": \"{path}\", \"cloud_path\": \"Game/{path}\", \"sha1\": \"{sha1}\", \"size\": 3, \"url\": \"{url}\"}}"
    )
}

/// A project directory (config, manifest, local assets) next to a tree to pack.
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// `Boards/a.png` and `b.png` are pushed and unchanged locally.
    fn new() -> Project {
        let p = Project {
            dir: tempfile::tempdir().unwrap(),
        };
        p.write("ttskit.toml", CONFIG);
        p.write_manifest(&[
            manifest_entry(
                "Boards/a.png",
                &ttskit_cloud::sha1_hex(b"alpha"),
                &format!("{URL}a"),
            ),
            manifest_entry(
                "b.png",
                &ttskit_cloud::sha1_hex(b"beta"),
                &format!("{URL}b"),
            ),
        ]);
        p.write("src/Boards/a.png", "alpha");
        p.write("src/b.png", "beta");
        p
    }

    fn write_manifest(&self, entries: &[String]) {
        self.write(
            "ttskit.manifest.json",
            &format!(
                "{{\"version\": 1, \"entries\": [{}]}}\n",
                entries.join(", ")
            ),
        );
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap()
    }

    fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    /// Writes `tree/` with placeholders in a JSON value, Lua and XML.
    fn write_tree(&self) {
        self.write(
            "tree/save.json",
            "{\"SaveName\": \"T\", \"LuaScript\": \"file:script.lua\", \"XmlUI\": \"file:ui.xml\", \"Image\": \"asset:{Boards/a.png}\"}",
        );
        self.write(
            "tree/script.lua",
            "img = 'asset:{Boards/a.png}'\nother = 'asset:{b.png}'\n",
        );
        self.write("tree/ui.xml", "<Image image=\"asset:{b.png}\"/>\n");
    }

    fn pack(&self, extra: &[&str]) -> Output {
        let mut args = vec!["save", "pack", "tree", "-o", "out.json"];
        args.extend_from_slice(extra);
        run_in(self.dir.path(), &args)
    }
}

#[test]
fn resolves_placeholders_in_json_lua_and_xml() {
    let p = Project::new();
    p.write_tree();
    let o = p.pack(&[]);
    assert!(o.status.success(), "{}", err(&o));
    let packed = p.read("out.json");
    assert!(packed.contains(&format!("{URL}a")), "{packed}");
    assert!(packed.contains(&format!("img = '{URL}a'")), "{packed}");
    assert!(
        packed.contains(&format!("image=\\\"{URL}b\\\"")),
        "{packed}"
    );
    assert!(!packed.contains("asset:{"), "{packed}");
    let text = out(&o);
    assert!(
        text.contains("Resolved 4 asset placeholders (2 distinct)"),
        "{text}"
    );
}

#[test]
fn missing_entries_are_all_listed_and_nothing_is_written() {
    let p = Project::new();
    p.write_tree();
    p.write(
        "tree/script.lua",
        "a = 'asset:{x/1.png}' b = 'asset:{y.png}'",
    );
    let o = p.pack(&[]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("x/1.png") && e.contains("y.png"), "{e}");
    assert!(!e.contains("no ttskit.toml found"), "{e}");
    assert!(!p.exists("out.json"));
}

#[test]
fn stale_entries_fail_unless_allowed() {
    let p = Project::new();
    p.write_tree();
    p.write("src/b.png", "changed");
    let o = p.pack(&[]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("b.png") && e.contains("stale"), "{e}");
    assert!(e.contains("--allow-stale"), "{e}");
    assert!(!p.exists("out.json"));

    let o = p.pack(&["--allow-stale"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("warning:"), "{}", err(&o));
    assert!(p.read("out.json").contains(&format!("{URL}b")));
}

#[test]
fn a_missing_local_file_is_not_stale() {
    let p = Project::new();
    p.write_tree();
    std::fs::remove_file(p.path("src/b.png")).unwrap();
    let o = p.pack(&[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(err(&o), "");
}

#[test]
fn dry_run_lists_placeholders_and_writes_nothing() {
    let p = Project::new();
    p.write_tree();
    p.write("src/b.png", "changed");
    let o = p.pack(&["--dry-run", "--allow-stale"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains(&format!("asset Boards/a.png -> {URL}a (2 uses)\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("asset b.png -> {URL}b (2 uses) [stale]\n")),
        "{text}"
    );
    assert!(!text.contains("Resolved"), "{text}");
    assert!(!p.exists("out.json"));
}

#[test]
fn json_output_lists_the_assets() {
    let p = Project::new();
    p.write_tree();
    let o = p.pack(&["--json"]);
    assert!(o.status.success(), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    let assets = v["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[0]["path"], "Boards/a.png");
    assert_eq!(assets[0]["url"], format!("{URL}a"));
    assert_eq!(assets[0]["stale"], false);
    assert_eq!(assets[0]["count"], 2);

    // Without placeholders the array is present and empty.
    p.write("plain/save.json", "{\"SaveName\": \"T\"}");
    let o = run_in(
        p.dir.path(),
        &["save", "pack", "plain", "-o", "plain.json", "--json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["assets"], serde_json::json!([]));
}

#[test]
fn placeholders_inside_required_modules_are_resolved() {
    let p = Project::new();
    p.write(
        "tree/save.json",
        "{\"SaveName\": \"T\", \"LuaScript\": \"file:script.lua\"}",
    );
    p.write("tree/script.lua", "require(\"shared\")");
    p.write("lib/shared.lua", "return 'asset:{b.png}'");
    let o = p.pack(&["--include", "lib"]);
    assert!(o.status.success(), "{}", err(&o));
    let packed = p.read("out.json");
    assert!(packed.contains(&format!("{URL}b")), "{packed}");
    assert!(!packed.contains("asset:{"), "{packed}");
}

#[test]
fn config_can_be_given_from_another_cwd() {
    let p = Project::new();
    p.write_tree();
    let elsewhere = tempfile::tempdir().unwrap();
    let tree = p.path("tree");
    let output = elsewhere.path().join("out.json");
    let config = p.path("ttskit.toml");
    let args = |config_args: &[&str]| {
        let mut v = vec![
            "save",
            "pack",
            tree.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ];
        v.extend_from_slice(config_args);
        v.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let run = |a: Vec<String>| {
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        run_in(elsewhere.path(), &refs)
    };

    // Without a config nothing resolves, and the error hints at it.
    let o = run(args(&[]));
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no ttskit.toml found"), "{}", err(&o));
    assert!(err(&o).contains("--config"), "{}", err(&o));

    let o = run(args(&["--config", config.to_str().unwrap()]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        std::fs::read_to_string(&output)
            .unwrap()
            .contains(&format!("{URL}a"))
    );
}

#[test]
fn output_without_placeholders_is_unchanged() {
    let p = Project::new();
    p.write("tree/save.json", "{\"SaveName\": \"T\"}");
    let o = p.pack(&[]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.starts_with("Packed "), "{text}");
    assert!(!text.contains("asset"), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
}

/// The text of all files below `dir`, concatenated.
fn tree_text(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            text.push_str(&tree_text(&path));
        } else {
            text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    text
}

fn save_text() -> String {
    format!(
        "{{\r\n  \"SaveName\": \"T\",\r\n  \"LuaScript\": \"u = '{URL}a'\",\r\n  \"ObjectStates\": [\r\n    {{\r\n      \"GUID\": \"aaaaaa\",\r\n      \"Nickname\": \"Deck\",\r\n      \"FaceURL\": \"{URL}b\",\r\n      \"BackURL\": \"{URL}a\"\r\n    }}\r\n  ]\r\n}}"
    )
}

#[test]
fn unpack_assets_then_pack_round_trips() {
    let p = Project::new();
    p.write("save.json", &save_text());
    let o = run_in(
        p.dir.path(),
        &["save", "unpack", "save.json", "-o", "tree", "--assets"],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("Replaced 3 URLs by asset placeholders"),
        "{}",
        out(&o)
    );
    let tree_save = tree_text(&p.path("tree"));
    assert!(tree_save.contains("asset:{b.png}"), "{tree_save}");
    assert!(!tree_save.contains(URL), "{tree_save}");

    let o = p.pack(&["--keep-eol"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(p.read("out.json"), save_text());
}

#[test]
fn unpack_without_assets_leaves_urls() {
    let p = Project::new();
    p.write("save.json", &save_text());
    let o = run_in(p.dir.path(), &["save", "unpack", "save.json", "-o", "tree"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!out(&o).contains("Replaced"), "{}", out(&o));
    let tree = tree_text(&p.path("tree"));
    assert!(tree.contains(URL), "{tree}");
    assert!(!tree.contains("asset:{"), "{tree}");

    let o = run_in(
        p.dir.path(),
        &["save", "unpack", "save.json", "-o", "tree2", "--json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["assets_mapped"], 0);
}

#[test]
fn unpack_assets_without_project_fails_and_config_needs_assets() {
    let p = Project::new();
    p.write("save.json", &save_text());
    let empty = tempfile::tempdir().unwrap();
    let save = p.path("save.json");
    let o = run_in(
        empty.path(),
        &[
            "save",
            "unpack",
            save.to_str().unwrap(),
            "-o",
            "tree",
            "--assets",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("ttskit.toml"), "{}", err(&o));
    assert!(!empty.path().join("tree").exists());

    let o = run_in(
        p.dir.path(),
        &[
            "save",
            "unpack",
            "save.json",
            "-o",
            "tree",
            "--config",
            "ttskit.toml",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
}

#[test]
fn unpack_maps_only_the_exact_url_spelling() {
    let p = Project::new();
    let other = format!("{}a", URL.replace("https:", "http:"));
    p.write(
        "save.json",
        &format!("{{\"SaveName\": \"T\", \"A\": \"{URL}a\", \"B\": \"{other}\"}}"),
    );
    let o = run_in(
        p.dir.path(),
        &["save", "unpack", "save.json", "-o", "tree", "--assets"],
    );
    assert!(o.status.success(), "{}", err(&o));
    let tree = tree_text(&p.path("tree"));
    assert!(tree.contains("asset:{Boards/a.png}"), "{tree}");
    assert!(tree.contains(&other), "{tree}");
}

#[test]
fn a_broken_project_config_only_matters_when_placeholders_need_it() {
    let p = Project::new();
    p.write("ttskit.toml", "this is not toml [");
    p.write("tree/save.json", "{\"SaveName\": \"T\"}");
    let o = p.pack(&[]);
    assert!(o.status.success(), "{}", err(&o));

    std::fs::remove_file(p.path("out.json")).unwrap();
    p.write("tree/save.json", "{\"Image\": \"asset:{b.png}\"}");
    let o = p.pack(&[]);
    assert!(!o.status.success());
    assert!(!p.exists("out.json"));
}
