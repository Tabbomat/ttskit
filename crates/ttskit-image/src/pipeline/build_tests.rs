//! Tests of planning, building and pruning, on small generated PNGs.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;

const VERSION: &str = "1.0.0";

fn write_png(path: &Path, width: u32, height: u32, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbaImage::from_pixel(width, height, image::Rgba([shade, 40, 90, 255]))
        .save(path)
        .unwrap();
}

fn dims(path: &Path) -> (u32, u32) {
    image::image_dimensions(path).unwrap()
}

fn pipeline(text: &str) -> Pipeline {
    parse_pipeline(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn rule(src: &str, out: &str, extra: &str) -> String {
    format!("[[image.pipeline]]\nsrc = \"{src}\"\nout = \"{out}\"\n{extra}\n")
}

fn plan_with(dir: &Path, p: &Pipeline, version: &str) -> Plan {
    let cache = Cache::load(dir).unwrap();
    plan(
        p,
        &Context {
            config_dir: dir,
            version,
        },
        &cache,
    )
    .unwrap()
}

/// load, plan, build, save: what the command does.
fn run_with(dir: &Path, p: &Pipeline, version: &str) -> (Plan, BuildReport) {
    let ctx = Context {
        config_dir: dir,
        version,
    };
    let mut cache = Cache::load(dir).unwrap();
    let pl = plan(p, &ctx, &cache).unwrap();
    let report = build(p, &ctx, &pl, &mut cache);
    cache.save(dir).unwrap();
    (pl, report)
}

fn run(dir: &Path, p: &Pipeline) -> (Plan, BuildReport) {
    run_with(dir, p, VERSION)
}

fn reason(job: &Job) -> Option<BuildReason> {
    match job.status {
        JobStatus::Build { reason } => Some(reason),
        JobStatus::UpToDate => None,
    }
}

fn job_for<'a>(plan: &'a Plan, src: &str) -> &'a Job {
    plan.jobs
        .iter()
        .find(|j| j.src == src)
        .unwrap_or_else(|| panic!("no job for {src}"))
}

fn all_up_to_date(plan: &Plan) -> bool {
    plan.jobs.iter().all(|j| j.status == JobStatus::UpToDate)
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_png(&dir.path().join("art/a.png"), 16, 8, 10);
    write_png(&dir.path().join("art/sub/b.png"), 16, 8, 20);
    dir
}

fn simple() -> Pipeline {
    pipeline(&rule("art/**/*.png", "out", ""))
}

// ---- plan ----

#[test]
fn plan_lists_new_outputs_and_writes_nothing() {
    let dir = project();
    let p = simple();
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(pl.jobs.len(), 2);
    for job in &pl.jobs {
        assert_eq!(reason(job), Some(BuildReason::New));
    }
    assert!(!dir.path().join("out").exists());
    assert!(!dir.path().join(CACHE_FILE_NAME).exists());
}

#[test]
fn subfolders_map_below_the_out_directory() {
    let dir = project();
    let (pl, report) = run(dir.path(), &simple());
    assert_eq!(job_for(&pl, "art/a.png").outputs, ["out/a.png"]);
    assert_eq!(job_for(&pl, "art/sub/b.png").outputs, ["out/sub/b.png"]);
    assert_eq!(report.built(), 2);
    assert!(dir.path().join("out/sub/b.png").is_file());
}

#[test]
fn missing_base_directory_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let pl = plan_with(dir.path(), &simple(), VERSION);
    assert!(pl.jobs.is_empty());
}

#[test]
fn cache_file_and_config_are_never_sources() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(CACHE_FILE_NAME), "x").unwrap();
    fs::write(dir.path().join("ttskit.toml"), "x").unwrap();
    let p = pipeline(&rule("*", "out", ""));
    let ctx = Context {
        config_dir: dir.path(),
        version: VERSION,
    };
    assert!(plan(&p, &ctx, &Cache::default()).unwrap().jobs.is_empty());
}

// ---- incremental ----

#[test]
fn second_build_skips_everything_and_touches_nothing() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    let out = dir.path().join("out/a.png");
    let bytes = fs::read(&out).unwrap();
    let mtime = fs::metadata(&out).unwrap().modified().unwrap();
    let cache_text = fs::read_to_string(dir.path().join(CACHE_FILE_NAME)).unwrap();

    assert!(all_up_to_date(&plan_with(dir.path(), &p, VERSION)));
    let (_, report) = run(dir.path(), &p);
    assert_eq!(report.up_to_date(), 2);
    assert_eq!(report.built(), 0);
    assert_eq!(fs::read(&out).unwrap(), bytes);
    assert_eq!(fs::metadata(&out).unwrap().modified().unwrap(), mtime);
    assert_eq!(
        fs::read_to_string(dir.path().join(CACHE_FILE_NAME)).unwrap(),
        cache_text
    );
}

#[test]
fn cache_file_is_sorted_pretty_json_with_a_newline() {
    let dir = project();
    run(dir.path(), &simple());
    let text = fs::read_to_string(dir.path().join(CACHE_FILE_NAME)).unwrap();
    assert!(text.ends_with("}\n"));
    let first = text.find("out/a.png").unwrap();
    let second = text.find("out/sub/b.png").unwrap();
    assert!(first < second);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["format_version"], 1);
    let entry = &value["outputs"]["out/a.png"];
    assert_eq!(entry["rule"], "art/**/*.png");
    assert_eq!(entry["version"], VERSION);
    for key in ["inputs", "options", "sha256"] {
        assert_eq!(entry[key].as_str().unwrap().len(), 64);
    }
    assert!(!dir.path().join("out/a.png.ttskit-tmp").exists());
}

#[test]
fn changed_input_rebuilds_only_that_job() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    write_png(&dir.path().join("art/a.png"), 16, 8, 99);
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(
        reason(job_for(&pl, "art/a.png")),
        Some(BuildReason::InputsChanged)
    );
    assert_eq!(reason(job_for(&pl, "art/sub/b.png")), None);
}

#[test]
fn changed_options_rebuild_the_rules_jobs() {
    let dir = project();
    run(
        dir.path(),
        &pipeline(&rule(
            "art/**/*.png",
            "out",
            "steps = [{ op = \"optimize\", level = 2 }]",
        )),
    );
    let changed = pipeline(&rule(
        "art/**/*.png",
        "out",
        "steps = [{ op = \"optimize\", level = 3 }]",
    ));
    let pl = plan_with(dir.path(), &changed, VERSION);
    for job in &pl.jobs {
        assert_eq!(reason(job), Some(BuildReason::OptionsChanged));
    }
}

#[test]
fn changed_version_rebuilds_everything() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    let pl = plan_with(dir.path(), &p, "2.0.0");
    for job in &pl.jobs {
        assert_eq!(reason(job), Some(BuildReason::VersionChanged));
    }
}

#[test]
fn deleted_output_is_rebuilt() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    fs::remove_file(dir.path().join("out/a.png")).unwrap();
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(
        reason(job_for(&pl, "art/a.png")),
        Some(BuildReason::OutputMissing)
    );
    let (_, report) = run(dir.path(), &p);
    assert_eq!(report.built(), 1);
    assert!(dir.path().join("out/a.png").is_file());
}

#[test]
fn hand_edited_output_is_overwritten() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    let out = dir.path().join("out/a.png");
    let original = fs::read(&out).unwrap();
    fs::write(&out, b"edited").unwrap();
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(
        reason(job_for(&pl, "art/a.png")),
        Some(BuildReason::OutputChanged)
    );
    run(dir.path(), &p);
    assert_eq!(fs::read(&out).unwrap(), original);
}

// ---- steps ----

#[test]
fn convert_changes_the_extension_and_resize_is_applied() {
    let dir = project();
    let p = pipeline(&rule(
        "art/**/*.png",
        "out",
        "steps = [{ op = \"resize\", max-side = 4 }, { op = \"convert\", to = \"jpg\" }]",
    ));
    let (pl, report) = run(dir.path(), &p);
    assert_eq!(report.failed(), 0);
    assert_eq!(job_for(&pl, "art/sub/b.png").outputs, ["out/sub/b.jpg"]);
    let out = dir.path().join("out/sub/b.jpg");
    assert_eq!(dims(&out), (4, 2));
    assert!(fs::read(&out).unwrap().starts_with(&[0xFF, 0xD8]));
}

#[test]
fn rule_without_steps_copies_the_file() {
    let dir = project();
    run(dir.path(), &simple());
    assert_eq!(
        fs::read(dir.path().join("art/a.png")).unwrap(),
        fs::read(dir.path().join("out/a.png")).unwrap()
    );
}

#[test]
fn a_failing_job_does_not_stop_the_others() {
    let dir = project();
    fs::write(dir.path().join("art/bad.png"), b"not an image").unwrap();
    let p = pipeline(&rule(
        "art/**/*.png",
        "out",
        "steps = [{ op = \"resize\", max-side = 4 }]",
    ));
    let (_, report) = run(dir.path(), &p);
    assert_eq!(report.failed(), 1);
    assert_eq!(report.built(), 2);
    let failed = report.jobs.iter().find(|j| j.error.is_some()).unwrap();
    assert_eq!(failed.src, "art/bad.png");
    assert!(dir.path().join("out/a.png").is_file());
    assert!(!dir.path().join("out/bad.png").exists());
    let cache = Cache::load(dir.path()).unwrap();
    assert!(cache.get("out/bad.png").is_none());
    assert!(cache.get("out/a.png").is_some());
}

#[test]
fn missing_pngquant_fails_the_plan() {
    let dir = project();
    let p = pipeline(&rule(
        "art/**/*.png",
        "out",
        "steps = [{ op = \"quantize\", engine = \"pngquant\", program = \"no-such-pngquant-xyz\" }]",
    ));
    let cache = Cache::default();
    let err = plan(
        &p,
        &Context {
            config_dir: dir.path(),
            version: VERSION,
        },
        &cache,
    )
    .unwrap_err();
    assert!(matches!(err, BuildError::Rules(_)), "{err}");
    assert!(err.to_string().contains("no-such-pngquant-xyz"), "{err}");
    assert!(!dir.path().join("out").exists());
}

// ---- conflicts ----

fn conflict_text(dir: &Path, p: &Pipeline) -> String {
    let cache = Cache::default();
    let err = plan(
        p,
        &Context {
            config_dir: dir,
            version: VERSION,
        },
        &cache,
    )
    .unwrap_err();
    assert!(matches!(err, BuildError::Conflicts(_)), "{err}");
    assert!(!dir.join("out").exists());
    assert!(!dir.join(CACHE_FILE_NAME).exists());
    err.to_string()
}

#[test]
fn two_rules_writing_the_same_output_conflict() {
    let dir = project();
    let text = format!(
        "{}{}",
        rule("art/a.png", "out", ""),
        rule("art/a.png", "out", "steps = [{ op = \"optimize\" }]")
    );
    assert!(conflict_text(dir.path(), &pipeline(&text)).contains("out/a.png"));
}

#[test]
fn outputs_conflict_regardless_of_case() {
    let dir = project();
    write_png(&dir.path().join("art2/A.png"), 4, 4, 1);
    let text = format!(
        "{}{}",
        rule("art/a.png", "out", ""),
        rule("art2/A.png", "out", "")
    );
    assert!(conflict_text(dir.path(), &pipeline(&text)).contains("more than one output"));
}

#[test]
fn an_output_matching_a_source_glob_conflicts() {
    let dir = project();
    let text = format!(
        "{}{}",
        rule("art/a.png", "art/sub", ""),
        rule("art/sub/*.png", "elsewhere", "")
    );
    let msg = conflict_text(dir.path(), &pipeline(&text));
    assert!(msg.contains("art/sub/a.png"), "{msg}");
    assert!(msg.contains("source"), "{msg}");
}

#[test]
fn an_output_on_an_existing_directory_conflicts() {
    let dir = project();
    fs::create_dir_all(dir.path().join("out/a.png")).unwrap();
    let msg = {
        let cache = Cache::default();
        plan(
            &simple(),
            &Context {
                config_dir: dir.path(),
                version: VERSION,
            },
            &cache,
        )
        .unwrap_err()
        .to_string()
    };
    assert!(msg.contains("existing directory"), "{msg}");
}

#[test]
fn an_output_named_like_the_cache_file_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("art")).unwrap();
    fs::write(dir.path().join("art/ttskit.toml"), b"x").unwrap();
    let p = pipeline(&rule("art/ttskit.toml", ".", ""));
    let msg = conflict_text(dir.path(), &p);
    assert!(msg.contains("cache file or the config"), "{msg}");
}

// ---- stale outputs ----

fn two_rules() -> Pipeline {
    pipeline(&format!(
        "{}{}",
        rule("art/a.png", "out", ""),
        rule("art/sub/*.png", "out2", "")
    ))
}

fn only_first_rule() -> Pipeline {
    pipeline(&rule("art/a.png", "out", ""))
}

#[test]
fn a_removed_rule_leaves_a_present_stale_output_until_pruned() {
    let dir = project();
    run(dir.path(), &two_rules());
    // A file that is not in the cache must never be deleted.
    fs::write(dir.path().join("out2/mine.txt"), b"mine").unwrap();

    let p = only_first_rule();
    let (pl, _) = run(dir.path(), &p);
    assert_eq!(pl.stale.len(), 1);
    assert_eq!(pl.stale[0].path, "out2/b.png");
    assert_eq!(pl.stale[0].state, StaleState::Present);
    assert!(dir.path().join("out2/b.png").is_file());
    assert!(Cache::load(dir.path()).unwrap().get("out2/b.png").is_some());
    // Still listed on the next plan.
    assert_eq!(plan_with(dir.path(), &p, VERSION).stale.len(), 1);

    let ctx = Context {
        config_dir: dir.path(),
        version: VERSION,
    };
    let mut cache = Cache::load(dir.path()).unwrap();
    let pl = plan(&p, &ctx, &cache).unwrap();
    let report = prune(&ctx, &pl, &mut cache);
    cache.save(dir.path()).unwrap();
    assert_eq!(report.removed, ["out2/b.png"]);
    assert!(!dir.path().join("out2/b.png").exists());
    assert!(dir.path().join("out2/mine.txt").is_file());
    assert!(Cache::load(dir.path()).unwrap().get("out2/b.png").is_none());
    assert!(plan_with(dir.path(), &p, VERSION).stale.is_empty());
}

#[test]
fn a_removed_source_makes_its_output_stale() {
    let dir = project();
    let p = simple();
    run(dir.path(), &p);
    fs::remove_file(dir.path().join("art/a.png")).unwrap();
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(pl.stale.len(), 1);
    assert_eq!(pl.stale[0].path, "out/a.png");
    assert_eq!(pl.stale[0].state, StaleState::Present);
    assert_eq!(pl.stale[0].rule, "art/**/*.png");
}

#[test]
fn prune_keeps_a_stale_file_changed_by_hand() {
    let dir = project();
    run(dir.path(), &two_rules());
    fs::write(dir.path().join("out2/b.png"), b"edited").unwrap();
    let p = only_first_rule();
    let ctx = Context {
        config_dir: dir.path(),
        version: VERSION,
    };
    let mut cache = Cache::load(dir.path()).unwrap();
    let pl = plan(&p, &ctx, &cache).unwrap();
    assert_eq!(pl.stale[0].state, StaleState::Changed);
    let report = prune(&ctx, &pl, &mut cache);
    assert!(report.removed.is_empty());
    assert_eq!(report.kept.len(), 1);
    assert_eq!(fs::read(dir.path().join("out2/b.png")).unwrap(), b"edited");
    assert!(cache.get("out2/b.png").is_none());
}

#[test]
fn a_stale_file_deleted_by_hand_is_gone_and_dropped_on_build() {
    let dir = project();
    run(dir.path(), &two_rules());
    fs::remove_file(dir.path().join("out2/b.png")).unwrap();
    let p = only_first_rule();
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(pl.stale[0].state, StaleState::Gone);
    // No prune: build alone drops the entry.
    run(dir.path(), &p);
    assert!(Cache::load(dir.path()).unwrap().get("out2/b.png").is_none());
    assert!(plan_with(dir.path(), &p, VERSION).stale.is_empty());
}

// ---- decks ----

fn deck_project(cards: u8) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for n in 1..=cards {
        write_png(&dir.path().join(format!("cards/c{n}.png")), 8, 8, n * 10);
    }
    write_png(&dir.path().join("art/hidden.png"), 8, 8, 200);
    dir
}

fn deck_pipeline() -> Pipeline {
    pipeline(&rule(
        "cards/*.png",
        "out/decks",
        "deck = { name = \"base\", hidden = \"art/hidden.png\", columns = 2, rows = 1 }",
    ))
}

#[test]
fn one_sheet_deck_is_named_without_a_number() {
    let dir = deck_project(1);
    let (pl, report) = run(dir.path(), &deck_pipeline());
    assert_eq!(report.failed(), 0, "{:?}", report.jobs);
    let job = &pl.jobs[0];
    assert_eq!(job.outputs, ["out/decks/base.png", "out/decks/base.json"]);
    assert_eq!(job.inputs, ["cards/c1.png", "art/hidden.png"]);
    assert_eq!(dims(&dir.path().join("out/decks/base.png")), (16, 8));
}

#[test]
fn several_sheets_are_numbered_and_the_description_has_relative_sources() {
    let dir = deck_project(2);
    let (pl, report) = run(dir.path(), &deck_pipeline());
    assert_eq!(report.failed(), 0, "{:?}", report.jobs);
    assert_eq!(
        pl.jobs[0].outputs,
        [
            "out/decks/base_1.png",
            "out/decks/base_2.png",
            "out/decks/base.json"
        ]
    );
    let text = fs::read_to_string(dir.path().join("out/decks/base.json")).unwrap();
    assert!(text.ends_with("}\n"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let sheets = value["sheets"].as_array().unwrap();
    assert_eq!(sheets[0]["file"], "base_1.png");
    assert_eq!(sheets[0]["cards"][0]["source"], "cards/c1.png");
    assert_eq!(sheets[1]["cards"][0]["source"], "cards/c2.png");
    assert!(!text.contains('\\'));
}

#[test]
fn the_hidden_image_is_an_input_of_the_deck() {
    let dir = deck_project(1);
    let p = deck_pipeline();
    run(dir.path(), &p);
    assert!(all_up_to_date(&plan_with(dir.path(), &p, VERSION)));
    write_png(&dir.path().join("art/hidden.png"), 8, 8, 77);
    let pl = plan_with(dir.path(), &p, VERSION);
    assert_eq!(reason(&pl.jobs[0]), Some(BuildReason::InputsChanged));
}

#[test]
fn adding_a_card_makes_the_single_sheet_stale() {
    let dir = deck_project(1);
    let p = deck_pipeline();
    run(dir.path(), &p);
    write_png(&dir.path().join("cards/c2.png"), 8, 8, 55);
    let (pl, report) = run(dir.path(), &p);
    assert_eq!(report.failed(), 0, "{:?}", report.jobs);
    assert_eq!(pl.stale.len(), 1);
    assert_eq!(pl.stale[0].path, "out/decks/base.png");
    assert_eq!(pl.stale[0].state, StaleState::Present);
    assert!(dir.path().join("out/decks/base_2.png").is_file());
}

#[test]
fn a_deck_without_cards_gets_a_note_and_no_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let pl = plan_with(dir.path(), &deck_pipeline(), VERSION);
    assert!(pl.jobs.is_empty());
    assert_eq!(pl.notes.len(), 1);
}

#[test]
fn deck_sheets_run_through_the_steps() {
    let dir = deck_project(1);
    let p = pipeline(&rule(
        "cards/*.png",
        "out",
        "steps = [{ op = \"optimize\" }]\ndeck = { name = \"base\", hidden = \"art/hidden.png\", columns = 2, rows = 1 }",
    ));
    let (_, report) = run(dir.path(), &p);
    assert_eq!(report.failed(), 0, "{:?}", report.jobs);
    assert!(dir.path().join("out/base.png").is_file());
}

// ---- cache file ----

#[test]
fn a_broken_cache_file_is_an_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(CACHE_FILE_NAME), "{ nope").unwrap();
    let err = Cache::load(dir.path()).unwrap_err().to_string();
    assert!(err.contains(CACHE_FILE_NAME), "{err}");
    assert!(err.contains("delete"), "{err}");
}

#[test]
fn a_cache_entry_outside_the_config_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(CACHE_FILE_NAME),
        r#"{"format_version":1,"outputs":{"../x.png":{"inputs":"","options":"","rule":"","sha256":"","version":""}}}"#,
    )
    .unwrap();
    assert!(Cache::load(dir.path()).is_err());
}
