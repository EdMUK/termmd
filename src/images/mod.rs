//! Loading, sizing and encoding images for terminals that can show them.
//!
//! Four backends, in descending order of fidelity:
//!
//! | Backend | Terminals | Notes |
//! |---|---|---|
//! | [`kitty`] | kitty, Ghostty, WezTerm, Konsole | PNG passthrough, exact placement |
//! | [`iterm2`] | iTerm2, WezTerm, mintty | inline file transfer |
//! | [`sixel`] | foot, mlterm, xterm, Windows Terminal | our own encoder and quantiser |
//! | [`blocks`] | anything with 256 colours | half blocks, always available |
//!
//! Everything funnels through [`Store`], which owns decoding, caching and the
//! choice of backend, and is the crate's [`ImageProvider`] implementation.

pub mod blocks;
pub mod iterm2;
pub mod kitty;
pub mod sixel;

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use image::{DynamicImage, ImageReader, imageops::FilterType};

use crate::markdown::ImageRef;
use crate::render::{ImageProvider, ImageRequest};
use crate::term::caps::{Capabilities, GraphicsProtocol};

/// Ceiling on decoded image size, to keep a hostile or careless file from
/// exhausting memory. 64 megapixels is far more than any terminal can show.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
/// Ceiling on a downloaded image.
const MAX_DOWNLOAD_BYTES: u64 = 32 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// A loaded image.
///
/// Vector images are kept as a tree rather than rasterised on load, so that
/// `encode` can render them at exactly the pixel size the terminal will show.
/// Rasterising once and then scaling would throw away the one advantage SVG
/// has, and badges are mostly text and thin strokes -- precisely what suffers.
enum Loaded {
    Raster(Box<DynamicImage>),
    #[cfg(feature = "svg")]
    Vector(Box<resvg::usvg::Tree>),
}

impl Loaded {
    fn size(&self) -> (u32, u32) {
        match self {
            Self::Raster(image) => (image.width(), image.height()),
            #[cfg(feature = "svg")]
            Self::Vector(tree) => {
                let size = tree.size();
                (size.width().ceil() as u32, size.height().ceil() as u32)
            }
        }
    }
}

/// Why an image could not be shown.
///
/// Carried all the way to the placeholder text. An image that silently fails to
/// appear is the single most confusing thing this program can do: the reader has
/// no way to tell a missing file from a blocked download from a format we cannot
/// read, and no idea which of them they could fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageProblem {
    /// A remote URL with `--remote-images` off.
    RemoteBlocked,
    /// The file is not where the document says it is.
    NotFound,
    /// A format we cannot decode.
    Unsupported(String),
    /// The download or the file failed.
    Unreadable(String),
    /// Bigger than we are willing to decode.
    TooLarge,
}

impl std::fmt::Display for ImageProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteBlocked => write!(f, "remote image, run with --remote-images"),
            Self::NotFound => write!(f, "file not found"),
            Self::Unsupported(what) => write!(f, "cannot read {what}"),
            Self::Unreadable(why) => write!(f, "{why}"),
            Self::TooLarge => write!(f, "image too large"),
        }
    }
}

/// Identifies one encoded form of one image.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EncodingKey {
    url: String,
    cols: u16,
    rows: u16,
    skip_top: u16,
    skip_bottom: u16,
    indent: u16,
}

/// Where images may be loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemotePolicy {
    /// Local files only. Remote URLs become placeholders.
    #[default]
    Deny,
    /// Fetch `http(s)` URLs.
    Allow,
}

/// Decodes, caches and encodes images.
pub struct Store {
    protocol: GraphicsProtocol,
    color: crate::term::caps::ColorDepth,
    cell_px: (u16, u16),
    /// Directory that relative image paths resolve against.
    base_dir: PathBuf,
    remote: RemotePolicy,
    /// Loaded images, keyed by URL. A failure is cached too, both so we do not
    /// retry it on every render pass (the pager re-renders on every resize) and
    /// so the renderer can say why the picture is not there.
    decoded: HashMap<String, Result<Loaded, ImageProblem>>,
    /// Encoded escape sequences, keyed by URL and requested cell size.
    encoded: HashMap<EncodingKey, Option<String>>,
    /// Incrementing ids for the kitty protocol.
    next_id: u32,
    /// Set when a remote image was skipped, so the CLI can mention the flag.
    pub skipped_remote: bool,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("protocol", &self.protocol.name())
            .field("cell_px", &self.cell_px)
            .field("cached", &self.decoded.len())
            .finish_non_exhaustive()
    }
}

impl Store {
    pub fn new(caps: &Capabilities, base_dir: impl Into<PathBuf>, remote: RemotePolicy) -> Self {
        Self {
            protocol: caps.graphics,
            color: caps.color,
            cell_px: caps.cell_px_or_guess(),
            base_dir: base_dir.into(),
            remote,
            decoded: HashMap::new(),
            encoded: HashMap::new(),
            next_id: 1,
            skipped_remote: false,
        }
    }

    /// Overrides the auto-detected protocol.
    pub fn with_protocol(mut self, protocol: GraphicsProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn protocol(&self) -> GraphicsProtocol {
        self.protocol
    }

    /// Clears cached encodings, keeping decoded pixels. Used when the terminal
    /// is resized and every image needs re-encoding at a new size.
    pub fn invalidate_encodings(&mut self) {
        self.encoded.clear();
    }

    /// Loads an image, caching the outcome either way.
    fn load(&mut self, url: &str) -> Result<&Loaded, ImageProblem> {
        if !self.decoded.contains_key(url) {
            let loaded = self
                .fetch_bytes(url)
                .and_then(|bytes| self.interpret(&bytes));
            self.decoded.insert(url.to_string(), loaded);
        }
        match self.decoded.get(url) {
            Some(Ok(loaded)) => Ok(loaded),
            Some(Err(problem)) => Err(problem.clone()),
            None => Err(ImageProblem::NotFound),
        }
    }

    /// Turns bytes into either pixels or a vector tree.
    fn interpret(&mut self, bytes: &[u8]) -> Result<Loaded, ImageProblem> {
        if looks_like_svg(bytes) {
            return self.load_svg(bytes);
        }
        decode(bytes).map(|image| Loaded::Raster(Box::new(image)))
    }

    #[cfg(feature = "svg")]
    fn load_svg(&mut self, bytes: &[u8]) -> Result<Loaded, ImageProblem> {
        let mut options = resvg::usvg::Options::default();
        // Badges are mostly text, so an SVG without fonts renders as an empty
        // shell. Loading the system fonts is slow enough to be worth doing once,
        // and only once an SVG has actually turned up.
        options.fontdb_mut().load_system_fonts();
        resvg::usvg::Tree::from_data(bytes, &options)
            .map(|tree| Loaded::Vector(Box::new(tree)))
            .map_err(|e| ImageProblem::Unreadable(format!("bad SVG: {e}")))
    }

    #[cfg(not(feature = "svg"))]
    fn load_svg(&mut self, _bytes: &[u8]) -> Result<Loaded, ImageProblem> {
        Err(ImageProblem::Unsupported(
            "SVG (built without the svg feature)".into(),
        ))
    }

    /// Reads the bytes behind a URL, path, or data URI.
    fn fetch_bytes(&mut self, url: &str) -> Result<Vec<u8>, ImageProblem> {
        if let Some(rest) = url.strip_prefix("data:") {
            return decode_data_uri(rest)
                .ok_or_else(|| ImageProblem::Unreadable("bad data URI".into()));
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            if self.remote == RemotePolicy::Deny {
                self.skipped_remote = true;
                return Err(ImageProblem::RemoteBlocked);
            }
            return fetch_remote(url);
        }
        let path = resolve_path(&self.base_dir, url).ok_or(ImageProblem::NotFound)?;
        let meta = std::fs::metadata(&path).map_err(|_| ImageProblem::NotFound)?;
        if meta.len() > MAX_DOWNLOAD_BYTES {
            return Err(ImageProblem::TooLarge);
        }
        std::fs::read(path).map_err(|e| ImageProblem::Unreadable(e.to_string()))
    }

    /// Why the image at `url` is not showing, if it has already been tried.
    pub fn recorded_problem(&self, url: &str) -> Option<ImageProblem> {
        match self.decoded.get(url) {
            Some(Err(problem)) => Some(problem.clone()),
            _ => None,
        }
    }
}

/// True if the bytes look like SVG rather than a bitmap.
fn looks_like_svg(bytes: &[u8]) -> bool {
    // Skip a UTF-8 BOM and any leading whitespace, then look for the root
    // element or an XML declaration. Sniffing beats trusting the extension: a
    // URL may have no extension at all.
    let start = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let head = &start[..start.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start();
    (trimmed.starts_with("<?xml")
        || trimmed.starts_with("<!DOCTYPE svg")
        || trimmed.starts_with("<svg"))
        && text.contains("<svg")
}

impl ImageProvider for Store {
    fn measure(&mut self, image: &ImageRef) -> Option<(u32, u32)> {
        self.load(&image.url).ok().map(Loaded::size)
    }

    fn problem(&mut self, image: &ImageRef) -> Option<String> {
        // Ask, which loads the image if it has not been tried yet, so that a
        // placeholder can explain itself even on the first pass.
        let _ = self.load(&image.url);
        self.recorded_problem(&image.url).map(|p| p.to_string())
    }

    fn encode(&mut self, request: ImageRequest<'_>) -> Option<String> {
        let key = EncodingKey {
            url: request.image.url.clone(),
            cols: request.cols,
            rows: request.rows,
            skip_top: request.skip_top,
            skip_bottom: request.skip_bottom,
            indent: request.indent,
        };
        if let Some(cached) = self.encoded.get(&key) {
            return cached.clone();
        }
        let result = self.encode_uncached(request);
        self.encoded.insert(key, result.clone());
        result
    }
}

impl Store {
    fn encode_uncached(&mut self, request: ImageRequest<'_>) -> Option<String> {
        let ImageRequest {
            image,
            cols,
            rows,
            skip_top,
            skip_bottom,
            indent,
        } = request;
        let visible_rows = request.visible_rows();
        if cols == 0 || visible_rows == 0 || self.protocol == GraphicsProtocol::None {
            return None;
        }
        let (cell_w, cell_h) = self.cell_px;
        let protocol = self.protocol;
        let id = self.next_id;

        // Blocks address cells directly, two pixel rows per cell; the pixel
        // protocols address real pixels.
        let (box_w, box_h, crop_unit) = match protocol {
            GraphicsProtocol::Blocks => (cols as u32, rows as u32 * 2, 2),
            _ => (
                cols as u32 * cell_w.max(1) as u32,
                rows as u32 * cell_h.max(1) as u32,
                cell_h.max(1) as u32,
            ),
        };

        let source = self.load(&image.url).ok()?;
        let resized = match source {
            Loaded::Raster(image) => fit(image, box_w, box_h),
            // Vector art is rendered straight at the target size: no scaling
            // step, so thin strokes and small text stay sharp.
            #[cfg(feature = "svg")]
            Loaded::Vector(tree) => rasterize_svg(tree, box_w, box_h)?,
        };
        let cropped = crop(
            &resized,
            skip_top as u32 * crop_unit,
            skip_bottom as u32 * crop_unit,
        );

        let sequence = match protocol {
            GraphicsProtocol::Kitty => kitty::encode(&cropped, cols, visible_rows, id),
            GraphicsProtocol::ITerm2 => iterm2::encode(&cropped, cols, visible_rows),
            GraphicsProtocol::Sixel => sixel::encode(&cropped.to_rgba8()),
            GraphicsProtocol::Blocks => Some(blocks::encode_with_depth(
                &cropped.to_rgba8(),
                indent,
                self.color,
            )),
            GraphicsProtocol::None => None,
        };
        if sequence.is_some() {
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }
        sequence
    }
}

/// Removes rows from the top and bottom of an image.
fn crop(image: &DynamicImage, top: u32, bottom: u32) -> DynamicImage {
    let height = image.height();
    if top == 0 && bottom == 0 {
        return image.clone();
    }
    // A crop that would leave nothing is treated as no crop; the caller has
    // already decided not to draw in that case.
    let remaining = height.saturating_sub(top).saturating_sub(bottom);
    if remaining == 0 {
        return image.clone();
    }
    image.crop_imm(0, top, image.width(), remaining)
}

/// Renders a vector tree to fill a box, preserving its aspect ratio.
#[cfg(feature = "svg")]
fn rasterize_svg(tree: &resvg::usvg::Tree, box_w: u32, box_h: u32) -> Option<DynamicImage> {
    use resvg::tiny_skia;

    let size = tree.size();
    let (sw, sh) = (size.width(), size.height());
    if sw <= 0.0 || sh <= 0.0 || box_w == 0 || box_h == 0 {
        return None;
    }
    let scale = (box_w as f32 / sw).min(box_h as f32 / sh);
    let (w, h) = (
        (sw * scale).round().max(1.0) as u32,
        (sh * scale).round().max(1.0) as u32,
    );

    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // tiny-skia stores premultiplied alpha; the image crate expects straight.
    let mut buffer = image::RgbaImage::new(w, h);
    for (pixel, out) in pixmap.pixels().iter().zip(buffer.pixels_mut()) {
        let c = pixel.demultiply();
        *out = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(DynamicImage::ImageRgba8(buffer))
}

/// Scales an image to fit inside a box without distorting it.
fn fit(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let (w, h) = (image.width().max(1), image.height().max(1));
    if w <= width && h <= height {
        return image.clone();
    }
    // CatmullRom keeps photographs sharp without Lanczos' cost on big inputs.
    image.resize(width.max(1), height.max(1), FilterType::CatmullRom)
}

/// Decodes image bytes with a guard on the decoded size.
fn decode(bytes: &[u8]) -> Result<DynamicImage, ImageProblem> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImageProblem::Unreadable(e.to_string()))?;
    // Naming the format makes "cannot read AVIF image" possible, which tells the
    // reader something they can act on.
    let format = reader.format();
    let describe = || match format {
        Some(f) => format!("{f:?} image"),
        None => "this image format".to_string(),
    };

    let (w, h) = reader
        .into_dimensions()
        .map_err(|_| ImageProblem::Unsupported(describe()))?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ImageProblem::TooLarge);
    }
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImageProblem::Unreadable(e.to_string()))?
        .decode()
        .map_err(|_| ImageProblem::Unsupported(describe()))
}

/// Decodes the payload of a `data:` URI.
fn decode_data_uri(rest: &str) -> Option<Vec<u8>> {
    let (meta, payload) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload.trim()))
            .ok()
    } else {
        Some(percent_decode(payload))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Resolves an image reference against the document's directory.
fn resolve_path(base: &Path, url: &str) -> Option<PathBuf> {
    let raw = url.split(['#', '?']).next().unwrap_or(url);
    let raw = raw.strip_prefix("file://").unwrap_or(raw);
    let decoded = String::from_utf8(percent_decode(raw)).unwrap_or_else(|_| raw.to_string());
    let path = Path::new(&decoded);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    full.exists().then_some(full)
}

/// Downloads an image over HTTP, with a timeout and a size cap.
fn fetch_remote(url: &str) -> Result<Vec<u8>, ImageProblem> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent(concat!("termmd/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| ImageProblem::Unreadable(short_http_error(&e)))?;
    let mut body = Vec::new();
    let mut reader = response.body_mut().as_reader().take(MAX_DOWNLOAD_BYTES);
    std::io::copy(&mut reader, &mut body).map_err(|e| ImageProblem::Unreadable(e.to_string()))?;
    Ok(body)
}

/// A one-line form of a request failure, for a placeholder that has to fit.
fn short_http_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        ureq::Error::Timeout(_) => "download timed out".into(),
        other => {
            let text = other.to_string();
            text.split(':')
                .next()
                .unwrap_or("download failed")
                .trim()
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A tiny PNG, built rather than checked in, so the tests carry no binaries.
    pub(crate) fn test_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                128,
                255,
            ]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn store_with(protocol: GraphicsProtocol, dir: &Path) -> Store {
        let caps = Capabilities {
            graphics: protocol,
            cell_px: Some((10, 20)),
            ..Default::default()
        };
        Store::new(&caps, dir, RemotePolicy::Deny)
    }

    #[test]
    fn loads_and_measures_a_local_image() {
        let dir = std::env::temp_dir().join("termmd-test-images");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.png"), test_png(40, 20)).unwrap();

        let mut store = store_with(GraphicsProtocol::Kitty, &dir);
        let img = ImageRef {
            url: "t.png".into(),
            ..Default::default()
        };
        assert_eq!(store.measure(&img), Some((40, 20)));
    }

    #[test]
    fn missing_images_fail_once_and_stay_failed() {
        let mut store = store_with(GraphicsProtocol::Kitty, Path::new("/nonexistent"));
        let img = ImageRef {
            url: "nope.png".into(),
            ..Default::default()
        };
        assert_eq!(store.measure(&img), None);
        assert_eq!(store.measure(&img), None);
        assert_eq!(store.decoded.len(), 1, "the failure should be cached");
    }

    #[test]
    fn refuses_remote_images_unless_allowed() {
        let mut store = store_with(GraphicsProtocol::Kitty, Path::new("."));
        let img = ImageRef {
            url: "https://example.com/x.png".into(),
            ..Default::default()
        };
        assert_eq!(store.measure(&img), None);
        assert!(
            store.skipped_remote,
            "should record that it skipped a remote image"
        );
    }

    #[test]
    fn decodes_base64_data_uris() {
        let png = test_png(4, 4);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let url = format!("data:image/png;base64,{b64}");
        let mut store = store_with(GraphicsProtocol::Kitty, Path::new("."));
        assert_eq!(
            store.measure(&ImageRef {
                url,
                ..Default::default()
            }),
            Some((4, 4))
        );
    }

    #[test]
    fn resolves_paths_relative_to_the_document() {
        let dir = std::env::temp_dir().join("termmd-test-rel/sub");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), test_png(2, 2)).unwrap();
        assert!(resolve_path(&dir, "a.png").is_some());
        assert!(resolve_path(&dir, "./a.png").is_some());
        assert!(
            resolve_path(&dir, "a.png?v=2").is_some(),
            "query strings should be ignored"
        );
        assert!(resolve_path(&dir, "missing.png").is_none());
    }

    #[test]
    fn encodes_for_each_protocol() {
        let dir = std::env::temp_dir().join("termmd-test-protocols");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.png"), test_png(40, 20)).unwrap();
        let img = ImageRef {
            url: "t.png".into(),
            ..Default::default()
        };

        for protocol in [
            GraphicsProtocol::Kitty,
            GraphicsProtocol::ITerm2,
            GraphicsProtocol::Sixel,
            GraphicsProtocol::Blocks,
        ] {
            let mut store = store_with(protocol, &dir);
            let seq = store.encode(ImageRequest::new(&img, 4, 2));
            assert!(seq.is_some(), "{} produced nothing", protocol.name());
            assert!(!seq.unwrap().is_empty());
        }

        let mut store = store_with(GraphicsProtocol::None, &dir);
        assert_eq!(store.encode(ImageRequest::new(&img, 4, 2)), None);
    }

    #[test]
    fn caches_encodings_per_size() {
        let dir = std::env::temp_dir().join("termmd-test-cache");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.png"), test_png(8, 8)).unwrap();
        let img = ImageRef {
            url: "t.png".into(),
            ..Default::default()
        };
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);

        store.encode(ImageRequest::new(&img, 4, 2));
        store.encode(ImageRequest::new(&img, 4, 2));
        store.encode(ImageRequest::new(&img, 8, 4));
        assert_eq!(store.encoded.len(), 2, "one entry per distinct size");

        store.invalidate_encodings();
        assert!(store.encoded.is_empty());
        assert_eq!(
            store.decoded.len(),
            1,
            "decoded pixels survive invalidation"
        );
    }

    #[test]
    fn cropping_shortens_the_drawn_image() {
        let dir = std::env::temp_dir().join("termmd-test-crop");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.png"), test_png(40, 80)).unwrap();
        let img = ImageRef {
            url: "t.png".into(),
            ..Default::default()
        };
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);

        let full = store.encode(ImageRequest::new(&img, 4, 4)).unwrap();
        let cropped = store
            .encode(ImageRequest {
                image: &img,
                cols: 4,
                rows: 4,
                skip_top: 2,
                skip_bottom: 0,
                indent: 0,
            })
            .unwrap();
        assert!(full.contains("r=4"), "full image should claim four rows");
        assert!(
            cropped.contains("r=2"),
            "a two-row crop should claim two rows: {cropped:.80}"
        );
        assert!(
            cropped.len() < full.len(),
            "cropped payload should be smaller"
        );
    }

    #[test]
    fn cropping_everything_draws_nothing() {
        let dir = std::env::temp_dir().join("termmd-test-crop2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.png"), test_png(8, 8)).unwrap();
        let img = ImageRef {
            url: "t.png".into(),
            ..Default::default()
        };
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);
        let seq = store.encode(ImageRequest {
            image: &img,
            cols: 2,
            rows: 2,
            skip_top: 2,
            skip_bottom: 0,
            indent: 0,
        });
        assert_eq!(seq, None);
    }

    #[test]
    fn fit_preserves_aspect_ratio() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 50));
        let out = fit(&img, 40, 40);
        assert_eq!((out.width(), out.height()), (40, 20));
    }

    #[test]
    fn fit_does_not_upscale() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(10, 10));
        let out = fit(&img, 100, 100);
        assert_eq!((out.width(), out.height()), (10, 10));
    }

    const BADGE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20">
        <rect width="60" height="20" fill="#4c1"/>
        <circle cx="30" cy="10" r="6" fill="#fff"/>
    </svg>"##;

    #[test]
    fn recognises_svg_by_its_content_not_its_name() {
        assert!(looks_like_svg(BADGE_SVG.as_bytes()));
        assert!(looks_like_svg(
            b"<?xml version=\"1.0\"?>\n<svg width=\"1\"></svg>"
        ));
        assert!(
            looks_like_svg(b"\xef\xbb\xbf<svg></svg>"),
            "a BOM should not hide it"
        );
        assert!(!looks_like_svg(&test_png(2, 2)));
        assert!(!looks_like_svg(b"<html><body>not svg</body></html>"));
        assert!(!looks_like_svg(b""));
    }

    #[cfg(feature = "svg")]
    #[test]
    fn renders_svg_at_the_size_it_will_be_shown() {
        let dir = std::env::temp_dir().join("termmd-test-svg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("badge.svg"), BADGE_SVG).unwrap();
        let img = ImageRef {
            url: "badge.svg".into(),
            ..Default::default()
        };

        let mut store = store_with(GraphicsProtocol::Kitty, &dir);
        assert_eq!(
            store.measure(&img),
            Some((60, 20)),
            "intrinsic size drives layout"
        );
        assert!(store.encode(ImageRequest::new(&img, 6, 2)).is_some());
        assert_eq!(store.recorded_problem("badge.svg"), None);
    }

    #[cfg(feature = "svg")]
    #[test]
    fn vector_images_are_rasterised_larger_when_shown_larger() {
        // The point of keeping the tree rather than a bitmap: no upscaling.
        let dir = std::env::temp_dir().join("termmd-test-svg-scale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("badge.svg"), BADGE_SVG).unwrap();
        let img = ImageRef {
            url: "badge.svg".into(),
            ..Default::default()
        };
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);

        let small = store.encode(ImageRequest::new(&img, 6, 2)).unwrap();
        let large = store.encode(ImageRequest::new(&img, 40, 13)).unwrap();
        assert!(
            large.len() > small.len(),
            "a bigger box should mean a bigger render"
        );
    }

    #[test]
    fn a_blocked_remote_image_says_how_to_allow_it() {
        let mut store = store_with(GraphicsProtocol::Kitty, Path::new("."));
        let img = ImageRef {
            url: "https://example.com/x.png".into(),
            ..Default::default()
        };
        assert_eq!(store.measure(&img), None);
        let reason = ImageProvider::problem(&mut store, &img).unwrap();
        assert!(
            reason.contains("--remote-images"),
            "unhelpful reason: {reason}"
        );
    }

    #[test]
    fn a_missing_file_says_so() {
        let mut store = store_with(GraphicsProtocol::Kitty, Path::new("/nonexistent"));
        let img = ImageRef {
            url: "gone.png".into(),
            ..Default::default()
        };
        let reason = ImageProvider::problem(&mut store, &img).unwrap();
        assert!(reason.contains("not found"), "got {reason}");
    }

    #[test]
    fn an_unreadable_format_names_itself() {
        let dir = std::env::temp_dir().join("termmd-test-badformat");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("x.png"),
            b"this is not a png at all, whatever it claims",
        )
        .unwrap();
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);
        let img = ImageRef {
            url: "x.png".into(),
            ..Default::default()
        };
        assert!(ImageProvider::problem(&mut store, &img).is_some());
    }

    #[test]
    fn a_working_image_reports_no_problem() {
        let dir = std::env::temp_dir().join("termmd-test-noproblem");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.png"), test_png(4, 4)).unwrap();
        let mut store = store_with(GraphicsProtocol::Kitty, &dir);
        let img = ImageRef {
            url: "ok.png".into(),
            ..Default::default()
        };
        assert!(ImageProvider::problem(&mut store, &img).is_none());
    }

    #[test]
    fn rejects_corrupt_image_data() {
        let problem = decode(b"not an image at all").unwrap_err();
        assert!(
            matches!(
                problem,
                ImageProblem::Unsupported(_) | ImageProblem::Unreadable(_)
            ),
            "got {problem:?}"
        );
    }
}
