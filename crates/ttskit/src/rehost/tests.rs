//! `save rehost` with an in-memory cloud and scripted HTTP clients.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

use ttskit_cloud::{BackupStore, CloudError, MemoryCloud, Op, UgcUrl};
use ttskit_core::{
    Download, DownloadError, DownloadResponse, Fetch, FetchConfig, FetchError, FetchMethod,
    FetchRequest, FetchResponse,
};

use super::*;
use crate::cloud::CommandError;

const PNG_A: &[u8] = b"\x89PNG\r\n\x1a\nimage a";
const PNG_B: &[u8] = b"\x89PNG\r\n\x1a\nimage b, a bit longer";
const PNG_O: &[u8] = b"\x89PNG\r\n\x1a\nother";

const A: &str = "https://i.imgur.com/a.png";
const B: &str = "https://i.imgur.com/b.png";
const SAME_AS_A: &str = "https://i.imgur.com/a-again.png";
const OTHER: &str = "https://example.com/o.png";
const DEAD: &str = "https://i.imgur.com/dead.png";

/// Serves bodies by URL for both `HEAD` and `GET`; an unknown URL is a 404.
#[derive(Default)]
struct Web {
    bodies: HashMap<String, Vec<u8>>,
    /// URLs whose `HEAD` has no `Content-Length`.
    no_length: Vec<String>,
    gets: Mutex<Vec<String>>,
    heads: Mutex<Vec<String>>,
}

impl Web {
    fn with(mut self, url: &str, body: &[u8]) -> Self {
        self.bodies.insert(url.to_owned(), body.to_vec());
        self
    }
    fn gets(&self) -> Vec<String> {
        self.gets.lock().unwrap().clone()
    }
    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

impl Fetch for Web {
    fn fetch(&self, request: &FetchRequest<'_>) -> Result<FetchResponse, FetchError> {
        assert_eq!(request.method, FetchMethod::Head);
        self.heads.lock().unwrap().push(request.url.to_owned());
        Ok(match self.bodies.get(request.url) {
            Some(body) => FetchResponse {
                status: 200,
                content_length: (!self.no_length.iter().any(|u| u == request.url))
                    .then_some(body.len() as u64),
                ..FetchResponse::default()
            },
            None => FetchResponse {
                status: 404,
                ..FetchResponse::default()
            },
        })
    }
}

impl Download for Web {
    fn download(
        &self,
        url: &str,
        _max_size: u64,
        sink: &mut dyn Write,
    ) -> Result<DownloadResponse, DownloadError> {
        self.gets.lock().unwrap().push(url.to_owned());
        match self.bodies.get(url) {
            Some(body) => {
                sink.write_all(body)
                    .map_err(|e| DownloadError::Io(e.to_string()))?;
                Ok(DownloadResponse {
                    status: 200,
                    content_type: Some("image/png".into()),
                    ..DownloadResponse::default()
                })
            }
            None => Ok(DownloadResponse {
                status: 404,
                ..DownloadResponse::default()
            }),
        }
    }
}

fn save_text(urls: &[&str]) -> String {
    let objects: Vec<String> = urls
        .iter()
        .enumerate()
        .map(|(i, u)| {
            format!(
                "    {{\r\n      \"GUID\": \"a{i:05}\",\r\n      \"Nickname\": \"Card {i}\",\r\n      \"FaceURL\": \"{u}\"\r\n    }}"
            )
        })
        .collect();
    format!(
        "{{\r\n  \"SaveName\": \"T\",\r\n  \"ObjectStates\": [\r\n{}\r\n  ]\r\n}}",
        objects.join(",\r\n")
    )
}

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(urls: &[&str]) -> Project {
        let p = Project {
            dir: tempfile::tempdir().unwrap(),
        };
        p.write(
            "ttskit.toml",
            "[cloud]\nfolder = \"MyGame\"\n[local]\ndir = \"assets\"\n",
        );
        p.write("save.json", &save_text(urls));
        std::fs::create_dir(p.path("assets")).unwrap();
        p
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path(name), text).unwrap();
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap()
    }
    fn options(&self) -> Options {
        Options {
            save: self.path("save.json"),
            config: self.path("ttskit.toml"),
            folder: DEFAULT_FOLDER.to_owned(),
            hosts: Vec::new(),
            skip_hosts: Vec::new(),
            out: None,
            in_place: false,
            in_scripts: false,
            yes: true,
            dry_run: false,
            fetch: FetchConfig {
                backoff: Duration::ZERO,
                ..FetchConfig::default()
            },
        }
    }
}

struct Run {
    result: Result<Status, String>,
    text: String,
}

fn run_with(
    options: Options,
    cloud: &mut MemoryCloud,
    web: &Web,
    answer: bool,
    backups: &Path,
) -> Run {
    let mut out = Vec::new();
    let result = prepare(options).and_then(|p| {
        let mut ask = |_: &str| -> Result<bool, CommandError> { Ok(answer) };
        run(
            &p,
            cloud,
            web,
            web,
            &BackupStore::new(backups),
            SystemTime::UNIX_EPOCH,
            "2026-01-01",
            &mut ask,
            &mut out,
        )
    });
    Run {
        result,
        text: String::from_utf8(out).unwrap(),
    }
}

fn run_yes(project: &Project, cloud: &mut MemoryCloud, web: &Web) -> Run {
    run_with(
        project.options(),
        cloud,
        web,
        true,
        &project.path("backups"),
    )
}

/// The URL values of the save, in order.
fn face_urls(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("\"FaceURL\": \""))
        .map(|l| l.trim_end_matches('"').to_owned())
        .collect()
}

/// How many asset files were written to the cloud (not index files).
fn writes(cloud: &MemoryCloud) -> usize {
    cloud
        .calls()
        .iter()
        .filter(|c| c.op == Op::Write && c.name.as_deref().is_some_and(|n| n.contains('_')))
        .count()
}

fn shared_urls(text: &str) -> Vec<UgcUrl> {
    face_urls(text)
        .iter()
        .map(|u| UgcUrl::parse(u).unwrap_or_else(|| panic!("not a cloud URL: {u}")))
        .collect()
}

#[test]
fn full_run_moves_the_files_and_rewrites_the_save() {
    let project = Project::new(&[A, B, OTHER]);
    let web = Web::default()
        .with(A, PNG_A)
        .with(B, PNG_B)
        .with(OTHER, PNG_O);
    let mut cloud = MemoryCloud::new();
    let r = run_yes(&project, &mut cloud, &web);
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);

    // The original is untouched; the result is next to it.
    assert_eq!(project.read("save.json"), save_text(&[A, B, OTHER]));
    let new = project.read("save.rehosted.json");
    let urls = shared_urls(&new);
    assert_eq!(urls.len(), 3);

    // Files are in the project and in the cloud, named after their content.
    let files: Vec<_> = std::fs::read_dir(project.path("assets/rehosted/files"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 3);
    assert!(files.iter().all(|f| f.ends_with(".png")));
    assert_eq!(writes(&cloud), 3);

    // The manifest links the local files to the new URLs.
    let manifest = project.read("ttskit.manifest.json");
    assert!(manifest.contains("rehosted/files/"), "{manifest}");
    assert!(manifest.contains("MyGame/rehosted/files/"), "{manifest}");
    for u in &urls {
        assert!(manifest.contains(&u.to_string()), "{manifest}");
    }
    assert!(r.text.contains("3 to rehost"), "{}", r.text);
    assert!(r.text.contains("Replaced 3 URLs"), "{}", r.text);
}

#[test]
fn urls_of_the_own_cloud_are_left_alone() {
    let project = Project::new(&[A]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new();
    assert_eq!(
        run_yes(&project, &mut cloud, &web).result,
        Ok(Status::Clean)
    );

    // Rehosting the result again finds only URLs of the own cloud.
    let second = Project::new(&[]);
    second.write("save.json", &project.read("save.rehosted.json"));
    let r = run_with(
        second.options(),
        &mut cloud,
        &Web::default(),
        true,
        &second.path("backups"),
    );
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    assert!(r.text.contains("1 already in your cloud"), "{}", r.text);
    assert!(r.text.contains("Nothing to rehost"), "{}", r.text);
    assert!(!second.path("save.rehosted.json").exists());
}

#[test]
fn identical_content_is_uploaded_once() {
    let project = Project::new(&[A, SAME_AS_A]);
    let web = Web::default().with(A, PNG_A).with(SAME_AS_A, PNG_A);
    let mut cloud = MemoryCloud::new();
    let r = run_yes(&project, &mut cloud, &web);
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    assert_eq!(writes(&cloud), 1);
    let urls = face_urls(&project.read("save.rehosted.json"));
    assert_eq!(urls.len(), 2);
    assert_eq!(urls[0], urls[1]);
    assert!(UgcUrl::parse(&urls[0]).is_some());
}

#[test]
fn a_dead_source_url_stays_and_gives_exit_status_2() {
    let project = Project::new(&[A, DEAD]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new();
    let r = run_yes(&project, &mut cloud, &web);
    assert_eq!(r.result, Ok(Status::Problems), "{}", r.text);
    assert!(r.text.contains(&format!("failed: {DEAD}")), "{}", r.text);
    let urls = face_urls(&project.read("save.rehosted.json"));
    assert!(UgcUrl::parse(&urls[0]).is_some());
    assert_eq!(urls[1], DEAD);
}

#[test]
fn nothing_downloadable_uploads_and_writes_nothing() {
    let project = Project::new(&[DEAD]);
    let mut cloud = MemoryCloud::new();
    let r = run_yes(&project, &mut cloud, &Web::default());
    assert_eq!(r.result, Ok(Status::Problems), "{}", r.text);
    assert_eq!(writes(&cloud), 0);
    assert!(!project.path("save.rehosted.json").exists());
}

#[test]
fn an_interrupted_run_is_finished_by_running_it_again() {
    let project = Project::new(&[A, B]);
    let web = Web::default().with(A, PNG_A).with(B, PNG_B);
    let mut cloud = MemoryCloud::new();
    // The first share fails: that file is written but not recorded.
    cloud.fail_once(Op::Share, None, CloudError::Backend("boom".into()));
    let r = run_yes(&project, &mut cloud, &web);
    let err = r.result.expect_err("the share failed");
    assert!(err.contains("run the same push again"), "{err}");
    assert!(!project.path("save.rehosted.json").exists());
    assert!(writes(&cloud) >= 1);
    assert_eq!(web.gets().len(), 2);

    // Same arguments again: no download, and no file is written twice.
    let r = run_yes(&project, &mut cloud, &web);
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    assert_eq!(web.gets().len(), 2, "files are not downloaded again");
    assert_eq!(writes(&cloud), 2, "each file is written once in total");
    let urls = shared_urls(&project.read("save.rehosted.json"));
    assert_eq!(urls.len(), 2);
    assert!(r.text.contains("2 to rehost"), "{}", r.text);
    assert!(r.text.contains("(downloaded)"), "{}", r.text);
}

#[test]
fn dry_run_downloads_and_changes_nothing() {
    let project = Project::new(&[A, B]);
    let mut web = Web::default().with(A, PNG_A).with(B, PNG_B);
    web.no_length.push(B.to_owned());
    let mut cloud = MemoryCloud::new();
    let mut options = project.options();
    options.dry_run = true;
    options.yes = false;
    let r = run_with(options, &mut cloud, &web, true, &project.path("backups"));
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    assert!(web.gets().is_empty());
    assert_eq!(web.heads().len(), 2);
    assert!(cloud.mutating_calls().is_empty());
    assert!(!project.path("assets/rehosted").exists());
    assert!(!project.path("save.rehosted.json").exists());
    assert!(!project.path("ttskit.manifest.json").exists());
    assert!(r.text.contains(A), "{}", r.text);
    assert!(
        r.text.contains(&format_size(PNG_A.len() as u64)),
        "{}",
        r.text
    );
    assert!(r.text.contains("size unknown"), "{}", r.text);
    assert!(r.text.contains("cloud: "), "{}", r.text);
    assert!(r.text.contains("MyGame/rehosted/files"), "{}", r.text);
    assert!(r.text.contains("Dry run"), "{}", r.text);
}

#[test]
fn saying_no_changes_nothing() {
    let project = Project::new(&[A]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new();
    let mut options = project.options();
    options.yes = false;
    let r = run_with(options, &mut cloud, &web, false, &project.path("backups"));
    assert_eq!(r.result, Ok(Status::Clean));
    assert!(r.text.contains("Cancelled"), "{}", r.text);
    assert!(web.gets().is_empty());
    assert!(cloud.mutating_calls().is_empty());
}

#[test]
fn host_filters_select_the_urls() {
    let web = Web::default().with(A, PNG_A).with(OTHER, PNG_O);

    let project = Project::new(&[A, OTHER]);
    let mut cloud = MemoryCloud::new();
    let mut options = project.options();
    options.hosts = vec!["imgur".into()];
    let r = run_with(options, &mut cloud, &web, true, &project.path("backups"));
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    let urls = face_urls(&project.read("save.rehosted.json"));
    assert!(UgcUrl::parse(&urls[0]).is_some());
    assert_eq!(urls[1], OTHER);

    let project = Project::new(&[A, OTHER]);
    let mut cloud = MemoryCloud::new();
    let mut options = project.options();
    options.skip_hosts = vec!["imgur.com".into()];
    let r = run_with(options, &mut cloud, &web, true, &project.path("backups"));
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    let urls = face_urls(&project.read("save.rehosted.json"));
    assert_eq!(urls[0], A);
    assert!(UgcUrl::parse(&urls[1]).is_some());
}

#[test]
fn the_quota_is_checked_before_anything_is_uploaded() {
    let project = Project::new(&[A]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new().with_quota(4);
    let r = run_yes(&project, &mut cloud, &web);
    let err = r.result.expect_err("does not fit");
    assert!(err.contains("not enough cloud space"), "{err}");
    assert!(!project.path("save.rehosted.json").exists());
    assert_eq!(writes(&cloud), 0);
}

#[test]
fn in_place_replaces_the_input() {
    let project = Project::new(&[A]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new();
    let mut options = project.options();
    options.in_place = true;
    let r = run_with(options, &mut cloud, &web, true, &project.path("backups"));
    assert_eq!(r.result, Ok(Status::Clean), "{}", r.text);
    assert!(UgcUrl::parse(&face_urls(&project.read("save.json"))[0]).is_some());
    assert!(!project.path("save.rehosted.json").exists());
}

#[test]
fn output_arguments_are_checked_before_anything_happens() {
    let project = Project::new(&[A]);

    let mut options = project.options();
    options.in_place = true;
    options.yes = false;
    let err = prepare(options).err().expect("needs --yes");
    assert!(err.contains("--in-place"), "{err}");

    project.write("save.rehosted.json", "old");
    let mut options = project.options();
    options.yes = false;
    let err = prepare(options).err().expect("exists");
    assert!(err.contains("already exists"), "{err}");

    for bad in ["", "..", "a/../b", "a:b", "."] {
        let mut options = project.options();
        options.folder = bad.to_owned();
        assert!(prepare(options).is_err(), "{bad:?}");
    }
}

#[test]
fn the_output_save_keeps_its_layout_except_the_urls() {
    let project = Project::new(&[A]);
    let web = Web::default().with(A, PNG_A);
    let mut cloud = MemoryCloud::new();
    assert_eq!(
        run_yes(&project, &mut cloud, &web).result,
        Ok(Status::Clean)
    );
    let new = project.read("save.rehosted.json");
    let expected = save_text(&[A]).replace(A, &face_urls(&new)[0]);
    assert_eq!(new, expected);
}
