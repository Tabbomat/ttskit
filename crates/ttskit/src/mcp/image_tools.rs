//! The image tool of the MCP server. It exists only in builds with the
//! `image` feature: other builds do not list it.

use std::path::{Path, PathBuf};

use rmcp::handler::server::router::tool::ToolRouter;
use serde::Deserialize;
use serde_json::Value;

use super::resolve_in_root;
use super::save_tools::json_of;
use crate::image;

/// Whether this build has image support.
pub(super) fn available() -> bool {
    cfg!(feature = "image")
}

/// Drops the image tool from `router` unless `available`, so that
/// `tools/list` omits it.
pub(super) fn with_image_tools<S>(mut router: ToolRouter<S>, available: bool) -> ToolRouter<S>
where
    S: Send + Sync + 'static,
{
    if !available {
        router.remove_route("image_check");
    }
    router
}

/// A byte count: a number, or text like "2m" (k, m and g are 1024-based).
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(untagged)]
pub(super) enum Bytes {
    Number(u64),
    Text(String),
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct ImageCheckArgs {
    /// Image files or directories (scanned recursively), relative to the server's root or absolute (must be inside the root).
    inputs: Vec<String>,
    /// Longest allowed side in pixels (at least 1; default 4096).
    max_side: Option<u32>,
    /// Largest allowed file size: a number of bytes, or text with a k, m or g suffix (1024-based, e.g. "2m"). Default: no limit.
    max_bytes: Option<Bytes>,
}

pub(super) fn check(root: &Path, args: ImageCheckArgs) -> Result<Value, String> {
    if args.inputs.is_empty() {
        return Err("inputs is empty; give at least one image file or directory".to_owned());
    }
    let max_side = args.max_side.unwrap_or(4096);
    if max_side == 0 {
        return Err("max_side must be at least 1".to_owned());
    }
    let max_bytes = match &args.max_bytes {
        None => None,
        Some(Bytes::Number(n)) => Some(*n),
        Some(Bytes::Text(text)) => Some(image::parse_bytes(text)?),
    };
    let inputs = args
        .inputs
        .iter()
        .map(|p| resolve_in_root(root, Path::new(p)))
        .collect::<Result<Vec<PathBuf>, _>>()?;
    let options = image::CheckArgs {
        inputs,
        max_side,
        max_bytes,
        json: true,
    };
    // Findings and unreadable files are in the JSON (`errors`), not tool errors.
    json_of(|out| image::check(&options, out).map(|_| ()))
}
