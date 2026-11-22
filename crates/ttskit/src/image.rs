//! `ttskit image`: image inspection. Needs the `image` cargo feature.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use serde::Serialize;

#[cfg(feature = "image")]
pub fn info(inputs: &[PathBuf], json: bool, out: &mut dyn Write) -> Result<bool, String> {
    let files = ttskit_image::collect_inputs(inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    let mut report = Report::default();
    for file in &files {
        match ttskit_image::inspect(file) {
            Ok(info) => report.images.push(info),
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    report.write(json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

#[cfg(not(feature = "image"))]
pub fn info(_inputs: &[PathBuf], _json: bool, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
#[derive(Default, Serialize)]
struct Report {
    images: Vec<ttskit_image::ImageInfo>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl Report {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for i in &self.images {
            let anim = if i.animated {
                format!(", {} frames", i.frames)
            } else {
                String::new()
            };
            let icc = match i.icc_profile_bytes {
                Some(n) => format!(", ICC profile ({n} bytes)"),
                None => String::new(),
            };
            writeln!(
                out,
                "{}: {} {}x{}, {} {}-bit{anim}{icc}, {} bytes",
                i.path, i.format, i.width, i.height, i.color, i.bit_depth, i.file_bytes
            )?;
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        writeln!(
            out,
            "{} inspected, {} failed",
            self.images.len(),
            self.errors.len()
        )
    }
}
