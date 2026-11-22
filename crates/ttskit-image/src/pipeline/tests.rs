use std::path::{Path, PathBuf};

use super::*;
use crate::{Engine, Filter, Fit, OutputFormat, ResizeTarget, StripMode};

/// A config with one rule built from `src`, `out` and extra rule lines.
fn rule(extra: &str) -> String {
    format!("[[image.pipeline]]\nsrc = \"a/*.png\"\nout = \"o\"\n{extra}\n")
}

fn step(inline: &str) -> String {
    rule(&format!("steps = [ {inline} ]"))
}

fn ok(text: &str) -> Pipeline {
    parse_pipeline(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn invalid_key(text: &str) -> String {
    match parse_pipeline(text) {
        Err(PipelineError::Invalid { key, .. }) => key,
        other => panic!("expected Invalid, got {other:?}\n{text}"),
    }
}

fn err_text(text: &str) -> String {
    parse_pipeline(text).unwrap_err().to_string()
}

fn only_step(inline: &str) -> Step {
    ok(&step(inline)).rules.remove(0).steps.remove(0)
}

/// A TOML basic string.
fn q(text: &str) -> String {
    let mut out = String::from('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const FULL: &str = r#"
[cloud]
folder = "G"
[local]
dir = "assets"

[[image.pipeline]]
src = "art/boards/**/*.png"
out = "assets/Boards"
steps = [
  { op = "resize", max-side = 4096 },
  { op = "optimize", level = 4 },
]

[[image.pipeline]]
src = ".\\art\\cards\\*.png"
out = "assets/Cards"
steps = [{ op = "quantize" }]
deck = { name = "base", hidden = "art/cards/hidden.png" }
"#;

#[test]
fn full_example_parses() {
    let p = ok(FULL);
    assert_eq!(p.rules.len(), 2);
    let r = &p.rules[0];
    assert_eq!(r.src, "art/boards/**/*.png");
    assert_eq!(r.base, Path::new("art/boards"));
    assert_eq!(r.out, Path::new("assets/Boards"));
    assert!(r.deck.is_none());
    match &r.steps[..] {
        [Step::Resize(rs), Step::Optimize(o)] => {
            assert_eq!(rs.target, ResizeTarget::MaxSide(4096));
            assert!(!rs.allow_enlarge);
            assert_eq!(rs.filter, Filter::Lanczos3);
            assert_eq!(rs.jpeg_quality, 85);
            assert_eq!(o.level, 4);
            assert_eq!(o.strip, StripMode::Safe);
        }
        other => panic!("{other:?}"),
    }
    let r = &p.rules[1];
    assert_eq!(r.src, "art/cards/*.png");
    assert_eq!(r.base, Path::new("art/cards"));
    let d = r.deck.as_ref().unwrap();
    assert_eq!(d.name, "base");
    assert_eq!(d.hidden.as_deref(), Some(Path::new("art/cards/hidden.png")));
    assert!(d.options.hidden.is_none());
    assert!(d.options.hidden_slot);
    assert_eq!(d.options.fill, [0x80; 3]);
    assert_eq!(d.options.format, OutputFormat::Png);
    assert_eq!(d.options.quality, 85);
}

#[test]
fn no_image_section_is_an_empty_pipeline() {
    assert!(ok("").rules.is_empty());
    assert!(
        ok("[cloud]\nfolder = \"G\"\n[local]\ndir = \"x\"\n")
            .rules
            .is_empty()
    );
    assert!(ok("[image]\n").rules.is_empty());
}

#[test]
fn cloud_and_local_are_not_checked() {
    let t = format!("[cloud]\nanything = 1\n[local]\nx = [1]\n{}", rule(""));
    assert_eq!(ok(&t).rules.len(), 1);
}

#[test]
fn unknown_keys_are_refused_by_name() {
    let cases = [
        ("prune = true\n".to_owned(), "prune"),
        ("[image]\nbogus = 1\n".to_owned(), "bogus"),
        (rule("extra = 1"), "extra"),
        (step("{ op = \"optimize\", levl = 1 }"), "levl"),
        (rule("deck = { name = \"d\", colums = 3 }"), "colums"),
    ];
    for (text, key) in cases {
        let e = err_text(&text);
        assert!(e.contains(key), "{e}");
        assert!(matches!(
            parse_pipeline(&text),
            Err(PipelineError::Syntax(_))
        ));
    }
}

#[test]
fn unknown_op_is_refused_by_name() {
    let e = err_text(&step("{ op = \"sharpen\" }"));
    assert!(e.contains("sharpen"), "{e}");
    let e = err_text(&step("{ level = 1 }"));
    assert!(e.contains("op"), "{e}");
}

#[test]
fn missing_src_and_out() {
    for t in [
        "[[image.pipeline]]\nout = \"o\"\n",
        "[[image.pipeline]]\nsrc = \"a\"\n",
    ] {
        let e = err_text(t);
        assert!(e.contains("missing field"), "{e}");
    }
}

#[test]
fn bad_paths() {
    let bad = [
        "a/../b", "..", "/abs", "\\abs", "C:/x", "C:\\x", "", "  ", "a\u{1}b",
    ];
    for b in bad {
        for field in ["src", "out"] {
            let (s, o) = if field == "src" {
                (b, "o")
            } else {
                ("a/*.png", b)
            };
            let t = format!(
                "[[image.pipeline]]\nsrc = \"a\"\nout = \"o\"\n[[image.pipeline]]\nsrc = {}\nout = {}\n",
                q(s),
                q(o)
            );
            assert_eq!(
                invalid_key(&t),
                format!("image.pipeline[1].{field}"),
                "{b:?}"
            );
        }
        let t = rule(&format!("deck = {{ name = \"d\", hidden = {} }}", q(b)));
        assert_eq!(invalid_key(&t), "image.pipeline[0].deck.hidden", "{b:?}");
    }
    let t = "[[image.pipeline]]\nsrc = \"/x\"\nout = \"o\"\n";
    let e = err_text(t);
    assert!(e.contains("must be relative to the config file"), "{e}");
    let e = err_text("[[image.pipeline]]\nsrc = \"a/../x\"\nout = \"o\"\n");
    assert!(e.contains("must not contain `..`"), "{e}");
    // src "." names nothing.
    assert_eq!(
        invalid_key("[[image.pipeline]]\nsrc = \"./\"\nout = \"o\"\n"),
        "image.pipeline[0].src"
    );
}

#[test]
fn paths_are_normalized() {
    let p = ok("[[image.pipeline]]\nsrc = \"./a\\\\b/*.png\"\nout = \"./x\\\\y\"\n");
    assert_eq!(p.rules[0].src, "a/b/*.png");
    assert_eq!(p.rules[0].out, Path::new("x/y"));
    for out in [".", "./"] {
        let p = ok(&format!("[[image.pipeline]]\nsrc = \"a\"\nout = {out:?}\n"));
        assert_eq!(p.rules[0].out, PathBuf::new(), "{out:?}");
    }
}

#[test]
fn glob_bases() {
    let base = |src: &str| {
        let t = format!("[[image.pipeline]]\nsrc = {src:?}\nout = \"o\"\n");
        ok(&t).rules[0].base.clone()
    };
    assert_eq!(base("art/boards/**/*.png"), Path::new("art/boards"));
    assert_eq!(base("**/*.png"), Path::new(""));
    assert_eq!(base("*.png"), Path::new(""));
    assert_eq!(base("art/a.png"), Path::new("art"));
    assert_eq!(base("a.png"), Path::new(""));
    assert_eq!(base("a/b/c/d.png"), Path::new("a/b/c"));
    assert_eq!(base("art/{x,y}/*.png"), Path::new("art"));
    assert_eq!(base("art/b?/z/*.png"), Path::new("art"));
}

#[test]
fn invalid_glob_is_refused() {
    let t = "[[image.pipeline]]\nsrc = \"a/{b\"\nout = \"o\"\n";
    assert_eq!(invalid_key(t), "image.pipeline[0].src");
}

#[test]
fn matching_is_case_insensitive_and_stops_at_slash() {
    let p = ok(&rule("").replace("a/*.png", "Art/*.PNG"));
    let r = &p.rules[0];
    assert!(r.matches("art/x.png"));
    assert!(r.matches("ART/X.Png"));
    assert!(!r.matches("art/sub/x.png"));
    assert!(!r.matches("x.png"));
    let p = ok(&rule("").replace("a/*.png", "a/**/*.png"));
    assert!(p.rules[0].matches("a/sub/deeper/x.png"));
    assert!(p.rules[0].matches("a/x.png"));
    let p = ok(&rule("").replace("a/*.png", "**/*.png"));
    assert!(p.rules[0].matches("x.png"));
    assert!(p.rules[0].matches("a/b/x.png"));
    let p = ok(&rule("").replace("a/*.png", "one.png"));
    assert!(p.rules[0].matches("ONE.png"));
    assert!(!p.rules[0].matches("d/one.png"));
}

#[test]
fn step_indexes_are_zero_based() {
    let t = step("{ op = \"optimize\" }, { op = \"optimize\", level = 7 }");
    assert_eq!(invalid_key(&t), "image.pipeline[0].steps[1].level");
}

#[test]
fn convert_step() {
    match only_step("{ op = \"convert\", to = \"png\" }") {
        Step::Convert(o) => {
            assert_eq!(o.to, OutputFormat::Png);
            assert_eq!(o.quality, 85);
            assert_eq!(o.background, None);
            assert!(!o.keep_metadata);
        }
        s => panic!("{s:?}"),
    }
    match only_step(
        "{ op = \"convert\", to = \"jpeg\", quality = 1, background = \"#fff\", keep-metadata = true }",
    ) {
        Step::Convert(o) => {
            assert_eq!(o.to, OutputFormat::Jpeg);
            assert_eq!(o.quality, 1);
            assert_eq!(o.background, Some([255, 255, 255]));
            assert!(o.keep_metadata);
        }
        s => panic!("{s:?}"),
    }
    let key = |s: &str| invalid_key(&step(s));
    let p = "image.pipeline[0].steps[0]";
    assert_eq!(key("{ op = \"convert\", to = \"gif\" }"), format!("{p}.to"));
    assert_eq!(
        key("{ op = \"convert\", to = \"jpg\", quality = 0 }"),
        format!("{p}.quality")
    );
    assert_eq!(
        key("{ op = \"convert\", to = \"jpg\", quality = 101 }"),
        format!("{p}.quality")
    );
    assert!(parse_pipeline(&step("{ op = \"convert\", to = \"jpg\", quality = 100 }")).is_ok());
    assert_eq!(
        key("{ op = \"convert\", to = \"jpg\", background = \"nope\" }"),
        format!("{p}.background")
    );
    // jpg only options on png
    assert_eq!(
        key("{ op = \"convert\", to = \"png\", quality = 80 }"),
        format!("{p}.quality")
    );
    assert_eq!(
        key("{ op = \"convert\", to = \"png\", background = \"white\" }"),
        format!("{p}.background")
    );
}

#[test]
fn resize_step() {
    let target = |s: &str| match only_step(s) {
        Step::Resize(o) => o.target,
        s => panic!("{s:?}"),
    };
    assert_eq!(
        target("{ op = \"resize\", max-side = 1 }"),
        ResizeTarget::MaxSide(1)
    );
    assert_eq!(
        target("{ op = \"resize\", width = 4294967295 }"),
        ResizeTarget::Width(u32::MAX)
    );
    assert_eq!(
        target("{ op = \"resize\", height = 7 }"),
        ResizeTarget::Height(7)
    );
    assert_eq!(
        target("{ op = \"resize\", scale = 2 }"),
        ResizeTarget::Scale(2.0)
    );
    assert_eq!(
        target("{ op = \"resize\", scale = 0.5 }"),
        ResizeTarget::Scale(0.5)
    );
    match only_step(
        "{ op = \"resize\", width = 9, allow-enlarge = true, filter = \"catmull-rom\", quality = 100 }",
    ) {
        Step::Resize(o) => {
            assert!(o.allow_enlarge);
            assert_eq!(o.filter, Filter::CatmullRom);
            assert_eq!(o.jpeg_quality, 100);
        }
        s => panic!("{s:?}"),
    }
    for (name, f) in [
        ("triangle", Filter::Triangle),
        ("nearest", Filter::Nearest),
        ("lanczos3", Filter::Lanczos3),
    ] {
        match only_step(&format!(
            "{{ op = \"resize\", width = 9, filter = {name:?} }}"
        )) {
            Step::Resize(o) => assert_eq!(o.filter, f),
            s => panic!("{s:?}"),
        }
    }
    let p = "image.pipeline[0].steps[0]";
    let key = |s: &str| invalid_key(&step(s));
    for (inline, expect) in [
        ("{ op = \"resize\", max-side = 0 }", "max-side"),
        ("{ op = \"resize\", max-side = -3 }", "max-side"),
        ("{ op = \"resize\", width = 0 }", "width"),
        ("{ op = \"resize\", width = 4294967296 }", "width"),
        ("{ op = \"resize\", height = 0 }", "height"),
        ("{ op = \"resize\", scale = 0 }", "scale"),
        ("{ op = \"resize\", scale = -1.5 }", "scale"),
        ("{ op = \"resize\", scale = nan }", "scale"),
        ("{ op = \"resize\", scale = inf }", "scale"),
        ("{ op = \"resize\", width = 1, quality = 0 }", "quality"),
        ("{ op = \"resize\", width = 1, quality = 101 }", "quality"),
        (
            "{ op = \"resize\", width = 1, filter = \"cubic\" }",
            "filter",
        ),
    ] {
        assert_eq!(key(inline), format!("{p}.{expect}"), "{inline}");
    }
    // none or several sizes
    for inline in [
        "{ op = \"resize\" }",
        "{ op = \"resize\", width = 1, height = 2 }",
        "{ op = \"resize\", max-side = 5, scale = 2 }",
    ] {
        assert_eq!(key(inline), p, "{inline}");
    }
    let e = err_text(&step("{ op = \"resize\", width = 1, height = 2 }"));
    assert!(e.contains("`width`") && e.contains("`height`"), "{e}");
}

#[test]
fn optimize_step() {
    match only_step("{ op = \"optimize\" }") {
        Step::Optimize(o) => {
            assert_eq!(o.level, 2);
            assert_eq!(o.strip, StripMode::Safe);
        }
        s => panic!("{s:?}"),
    }
    for (name, m) in [
        ("safe", StripMode::Safe),
        ("all", StripMode::All),
        ("none", StripMode::None),
    ] {
        match only_step(&format!(
            "{{ op = \"optimize\", level = 0, strip = {name:?} }}"
        )) {
            Step::Optimize(o) => {
                assert_eq!(o.level, 0);
                assert_eq!(o.strip, m);
            }
            s => panic!("{s:?}"),
        }
    }
    assert!(parse_pipeline(&step("{ op = \"optimize\", level = 6 }")).is_ok());
    let p = "image.pipeline[0].steps[0]";
    assert_eq!(
        invalid_key(&step("{ op = \"optimize\", level = 7 }")),
        format!("{p}.level")
    );
    assert_eq!(
        invalid_key(&step("{ op = \"optimize\", level = -1 }")),
        format!("{p}.level")
    );
    assert_eq!(
        invalid_key(&step("{ op = \"optimize\", strip = \"most\" }")),
        format!("{p}.strip")
    );
}

#[test]
fn quantize_step() {
    match only_step("{ op = \"quantize\" }") {
        Step::Quantize(o) => {
            assert_eq!(o.colors, 256);
            assert_eq!(o.quality, None);
            assert_eq!(o.dither, 1.0);
            assert_eq!(o.level, 2);
            assert_eq!(o.strip, StripMode::Safe);
            assert_eq!(o.engine, Engine::Builtin);
        }
        s => panic!("{s:?}"),
    }
    match only_step(
        "{ op = \"quantize\", colors = 2, quality = \"60-90\", dither = 0, level = 6, strip = \"all\", engine = \"pngquant\" }",
    ) {
        Step::Quantize(o) => {
            assert_eq!(o.colors, 2);
            assert_eq!(o.quality, Some((60, 90)));
            assert_eq!(o.dither, 0.0);
            assert_eq!(o.level, 6);
            assert_eq!(o.strip, StripMode::All);
            assert_eq!(o.engine, Engine::Pngquant { program: None });
        }
        s => panic!("{s:?}"),
    }
    // program implies pngquant; a relative path stays relative
    match only_step(
        "{ op = \"quantize\", colors = 256, dither = 1.0, program = \"tools/pngquant.exe\" }",
    ) {
        Step::Quantize(o) => assert_eq!(
            o.engine,
            Engine::Pngquant {
                program: Some(PathBuf::from("tools/pngquant.exe"))
            }
        ),
        s => panic!("{s:?}"),
    }
    let p = "image.pipeline[0].steps[0]";
    for (inline, expect) in [
        ("colors = 1", "colors"),
        ("colors = 257", "colors"),
        ("quality = \"90\"", "quality"),
        ("quality = \"90-60\"", "quality"),
        ("quality = \"0-101\"", "quality"),
        ("dither = -0.1", "dither"),
        ("dither = 1.1", "dither"),
        ("level = 7", "level"),
        ("strip = \"x\"", "strip"),
        ("engine = \"magick\"", "engine"),
        ("engine = \"builtin\", program = \"p\"", "program"),
        ("program = \"\"", "program"),
    ] {
        assert_eq!(
            invalid_key(&step(&format!("{{ op = \"quantize\", {inline} }}"))),
            format!("{p}.{expect}"),
            "{inline}"
        );
    }
    assert!(parse_pipeline(&step("{ op = \"quantize\", colors = 256, dither = 1 }")).is_ok());
    let e = err_text(&step(
        "{ op = \"quantize\", engine = \"builtin\", program = \"p\" }",
    ));
    assert!(e.contains("builtin"), "{e}");
}

fn deck(inline: &str) -> String {
    rule(&format!("deck = {{ {inline} }}"))
}

fn deck_rule(inline: &str) -> DeckRule {
    ok(&deck(inline)).rules.remove(0).deck.unwrap()
}

#[test]
fn deck_options() {
    let d = deck_rule("name = \"n\"");
    assert_eq!(d.name, "n");
    assert!(d.hidden.is_none());
    assert_eq!(d.options.columns, None);
    assert_eq!(d.options.card_size, None);
    assert_eq!(d.options.fit, None);
    assert_eq!(d.options.max_sheet_side, None);

    let d = deck_rule(
        "name = \"n\", hidden = \".\\\\h/back.png\", columns = 10, rows = 7, card-size = \"300x420\", \
         fit = \"cover\", fill = \"black\", max-sheet-side = 4096, to = \"jpg\", quality = 70",
    );
    assert_eq!(d.hidden.as_deref(), Some(Path::new("h/back.png")));
    assert_eq!(d.options.columns, Some(10));
    assert_eq!(d.options.rows, Some(7));
    assert_eq!(d.options.card_size, Some((300, 420)));
    assert_eq!(d.options.fit, Some(Fit::Cover));
    assert_eq!(d.options.fill, [0, 0, 0]);
    assert_eq!(d.options.max_sheet_side, Some(4096));
    assert_eq!(d.options.format, OutputFormat::Jpeg);
    assert_eq!(d.options.quality, 70);
    assert!(d.options.hidden.is_none());

    for (name, f) in [("contain", Fit::Contain), ("stretch", Fit::Stretch)] {
        let d = deck_rule(&format!("name = \"n\", fit = {name:?}"));
        assert_eq!(d.options.fit, Some(f));
    }
    let d = deck_rule("name = \"n\", columns = 1, rows = 1, hidden-slot = false");
    assert_eq!((d.options.columns, d.options.rows), (Some(1), Some(1)));
    assert!(!d.options.hidden_slot);
}

#[test]
fn deck_errors() {
    let p = "image.pipeline[0].deck";
    for (inline, expect) in [
        ("name = \"a/b\"", "name"),
        ("name = \"\"", "name"),
        ("name = \"..\"", "name"),
        ("name = \"a:b\"", "name"),
        ("name = \"n\", columns = 0", "columns"),
        ("name = \"n\", columns = 11", "columns"),
        ("name = \"n\", rows = 0", "rows"),
        ("name = \"n\", rows = 8", "rows"),
        ("name = \"n\", card-size = \"0x5\"", "card-size"),
        ("name = \"n\", card-size = \"abc\"", "card-size"),
        ("name = \"n\", fit = \"squash\"", "fit"),
        ("name = \"n\", fill = \"zzz\"", "fill"),
        ("name = \"n\", max-sheet-side = 0", "max-sheet-side"),
        ("name = \"n\", to = \"gif\"", "to"),
        ("name = \"n\", quality = 50", "quality"),
        ("name = \"n\", to = \"png\", quality = 50", "quality"),
        ("name = \"n\", to = \"jpg\", quality = 0", "quality"),
        ("name = \"n\", to = \"jpg\", quality = 101", "quality"),
        (
            "name = \"n\", hidden = \"h.png\", hidden-slot = false",
            "hidden",
        ),
        ("name = \"n\", hidden = \"../h.png\"", "hidden"),
    ] {
        assert_eq!(
            invalid_key(&deck(inline)),
            format!("{p}.{expect}"),
            "{inline}"
        );
    }
    let e = err_text("[[image.pipeline]]\nsrc = \"a\"\nout = \"o\"\ndeck = {}\n");
    assert!(e.contains("name"), "{e}");
}

#[test]
fn deck_rule_refuses_a_convert_step() {
    let t = rule(
        "steps = [{ op = \"optimize\" }, { op = \"convert\", to = \"jpg\" }]\ndeck = { name = \"n\" }",
    );
    assert_eq!(invalid_key(&t), "image.pipeline[0].steps[1]");
    let e = err_text(&t);
    assert!(e.contains("deck.to"), "{e}");
    // the same step without a deck is fine
    assert!(parse_pipeline(&step("{ op = \"convert\", to = \"jpg\" }")).is_ok());
}

#[test]
fn fingerprints() {
    let fp = |inline: &str| only_step(inline).fingerprint();
    // explicit defaults equal omitted ones
    assert_eq!(
        fp("{ op = \"optimize\" }"),
        fp("{ op = \"optimize\", level = 2, strip = \"safe\" }")
    );
    assert_eq!(
        fp("{ op = \"resize\", width = 3 }"),
        fp(
            "{ op = \"resize\", width = 3, allow-enlarge = false, filter = \"lanczos3\", quality = 85 }"
        )
    );
    assert_eq!(
        fp("{ op = \"quantize\" }"),
        fp(
            "{ op = \"quantize\", colors = 256, dither = 1.0, level = 2, strip = \"safe\", engine = \"builtin\" }"
        )
    );
    assert_eq!(
        fp("{ op = \"convert\", to = \"jpg\" }"),
        fp("{ op = \"convert\", to = \"jpeg\", quality = 85, keep-metadata = false }")
    );
    // a changed option changes the fingerprint
    let all = [
        fp("{ op = \"optimize\" }"),
        fp("{ op = \"optimize\", level = 3 }"),
        fp("{ op = \"optimize\", strip = \"all\" }"),
        fp("{ op = \"resize\", width = 3 }"),
        fp("{ op = \"resize\", width = 4 }"),
        fp("{ op = \"resize\", height = 3 }"),
        fp("{ op = \"resize\", width = 3, filter = \"nearest\" }"),
        fp("{ op = \"quantize\" }"),
        fp("{ op = \"quantize\", colors = 16 }"),
        fp("{ op = \"quantize\", program = \"x\" }"),
        fp("{ op = \"convert\", to = \"png\" }"),
        fp("{ op = \"convert\", to = \"jpg\" }"),
        fp("{ op = \"convert\", to = \"jpg\", quality = 80 }"),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }

    let d = |inline: &str| deck_rule(inline).fingerprint();
    assert_eq!(
        d("name = \"n\""),
        d("name = \"n\", hidden-slot = true, fill = \"808080\", to = \"png\"")
    );
    assert_ne!(d("name = \"n\""), d("name = \"m\""));
    assert_ne!(d("name = \"n\""), d("name = \"n\", hidden = \"h.png\""));
    assert_ne!(d("name = \"n\""), d("name = \"n\", columns = 3"));
    assert_ne!(d("name = \"n\""), d("name = \"n\", fill = \"000\""));
}

#[test]
fn syntax_error_has_position() {
    let e = parse_pipeline("[image\nx = 1").unwrap_err();
    assert!(matches!(e, PipelineError::Syntax(_)));
    assert!(e.to_string().starts_with("invalid config: "), "{e}");
    assert!(e.to_string().contains("line 1"), "{e}");
}

#[test]
fn wrong_types_are_syntax_errors() {
    assert!(matches!(
        parse_pipeline(&step("{ op = \"optimize\", level = \"high\" }")),
        Err(PipelineError::Syntax(_))
    ));
    assert!(matches!(
        parse_pipeline("[image]\npipeline = 3\n"),
        Err(PipelineError::Syntax(_))
    ));
}

#[test]
fn invalid_display() {
    let e = parse_pipeline(&step("{ op = \"optimize\", level = 9 }")).unwrap_err();
    assert!(
        e.to_string()
            .starts_with("invalid config: `image.pipeline[0].steps[0].level` must be"),
        "{e}"
    );
}
