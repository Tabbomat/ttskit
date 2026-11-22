//! File type detection for downloaded assets.

/// URL path extensions accepted as the file type (lowercase), with the
/// extension they are stored under.
const URL_EXTENSIONS: &[(&str, &str)] = &[
    ("obj", "obj"),
    ("mtl", "mtl"),
    ("fbx", "fbx"),
    ("txt", "txt"),
    ("json", "json"),
    ("mp3", "mp3"),
    ("ogg", "ogg"),
    ("wav", "wav"),
    ("mp4", "mp4"),
    ("ogv", "ogv"),
    ("pdf", "pdf"),
    ("unity3d", "unity3d"),
    ("png", "png"),
    ("jpg", "jpg"),
    ("jpeg", "jpg"),
    ("gif", "gif"),
    ("webp", "webp"),
];

/// The file extension for a download, from `prefix` (the first bytes of the
/// body), the `Content-Type` header and the URL, in that order of trust.
/// Falls back to `bin`. The result is the only text derived from the
/// network that reaches a file name, and it is always from a fixed list.
pub fn detect_type(prefix: &[u8], content_type: Option<&str>, url: &str) -> &'static str {
    if let Some(t) = from_magic(prefix) {
        return t;
    }
    let url_ext = url_extension(url);
    if let Some(t) = from_content_type(content_type, url_ext.as_deref()) {
        return t;
    }
    if let Some(ext) = url_ext
        && let Some((_, stored)) = URL_EXTENSIONS.iter().find(|(e, _)| *e == ext)
    {
        return stored;
    }
    "bin"
}

fn from_magic(p: &[u8]) -> Option<&'static str> {
    if p.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if p.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if p.starts_with(b"GIF87a") || p.starts_with(b"GIF89a") {
        Some("gif")
    } else if p.starts_with(b"RIFF") && p.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else if p.starts_with(b"RIFF") && p.get(8..12) == Some(b"WAVE") {
        Some("wav")
    } else if p.starts_with(b"%PDF") {
        Some("pdf")
    } else if p.starts_with(b"OggS") {
        Some("ogg")
    } else if p.starts_with(b"ID3") || matches!(p, [0xFF, 0xFB | 0xF3 | 0xF2, ..]) {
        Some("mp3")
    } else if p.get(4..8) == Some(b"ftyp") {
        Some("mp4")
    } else if p.starts_with(b"UnityFS") || p.starts_with(b"UnityWeb") || p.starts_with(b"UnityRaw")
    {
        Some("unity3d")
    } else {
        None
    }
}

fn from_content_type(content_type: Option<&str>, url_ext: Option<&str>) -> Option<&'static str> {
    let ct = content_type?.trim().to_ascii_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    match ct {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/ogg" | "application/ogg" => Some("ogg"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "video/mp4" => Some("mp4"),
        "application/pdf" => Some("pdf"),
        "model/obj" => Some("obj"),
        "text/plain" if url_ext == Some("obj") => Some("obj"),
        _ => None,
    }
}

/// Lowercase extension of the last path segment, without query or fragment.
fn url_extension(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let (_, path) = path.split_once('/')?;
    let name = path.rsplit('/').next().unwrap_or("");
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}
