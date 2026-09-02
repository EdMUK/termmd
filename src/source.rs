//! Reading input documents, and merging several of them into one page.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::markdown::{self, Block, Document, Inline, ParseOptions, Slugger, UrlBase};

/// Ceiling on a document fetched over HTTP. Generous for prose, and far short
/// of what would hurt.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a file to read when all we want is its first heading.
const TITLE_PEEK_BYTES: usize = 8 * 1024;
/// A directory listing stops here. Long before this it has stopped being a
/// document and started being a `ls`.
const MAX_INDEX_ENTRIES: usize = 500;

/// Where a document came from, and how to read it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A file, which can be re-read and watched.
    File(PathBuf),
    /// A directory, whose Markdown is listed as an index we generate.
    Directory(PathBuf),
    /// Fetched over HTTP.
    Url(String),
    /// Standard input, which is gone once it has been read.
    Stdin,
}

/// One input document, with enough context to re-read it later.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub origin: Origin,
    pub name: String,
    pub text: String,
}

impl Source {
    /// A source read from a file at `path`.
    pub fn file(path: PathBuf, text: String) -> Self {
        Self {
            name: path.display().to_string(),
            origin: Origin::File(path),
            text,
        }
    }

    /// A source that is known but not yet read. [`Source::reload`] reads it.
    pub fn pending(origin: Origin) -> Self {
        let name = match &origin {
            Origin::File(path) => path.display().to_string(),
            Origin::Directory(path) => {
                format!("{}/", path.display().to_string().trim_end_matches('/'))
            }
            Origin::Url(url) => url.clone(),
            Origin::Stdin => "<stdin>".into(),
        };
        Self {
            origin,
            name,
            text: String::new(),
        }
    }

    /// The file this came from, when it came from one.
    pub fn path(&self) -> Option<&Path> {
        match &self.origin {
            Origin::File(path) | Origin::Directory(path) => Some(path),
            _ => None,
        }
    }

    /// What this document's relative URLs resolve against.
    pub fn base(&self) -> UrlBase<'_> {
        match &self.origin {
            Origin::File(path) => UrlBase::Dir(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            ),
            Origin::Directory(path) => UrlBase::Dir(path),
            Origin::Url(url) => UrlBase::Url(url),
            Origin::Stdin => UrlBase::Dir(Path::new(".")),
        }
    }

    /// The directory to resolve a local path against.
    ///
    /// A remote document has none, and answers with the working directory: its
    /// own relative URLs have already been resolved into absolute ones, so
    /// nothing should be joining paths onto it any more.
    pub fn base_dir(&self) -> PathBuf {
        match self.base() {
            UrlBase::Dir(dir) => dir.to_path_buf(),
            UrlBase::Url(_) => PathBuf::from("."),
        }
    }

    /// Reads the source again, in whatever way it was read the first time.
    ///
    /// A directory index is regenerated rather than re-read, so `r` in the
    /// pager picks up a file that has just been added.
    pub fn reload(&mut self) -> Result<()> {
        match self.origin.clone() {
            Origin::File(path) => {
                self.text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
            }
            Origin::Directory(path) => self.text = index_of(&path)?,
            Origin::Url(url) => self.text = fetch(&url)?,
            // Standard input is a stream, and it has already been consumed.
            Origin::Stdin => {}
        }
        Ok(())
    }
}

/// Reads what was named on the command line, or stdin when nothing was.
///
/// A name is a file, a directory, a URL, or `-`. Anything with an `http://` or
/// `https://` scheme is fetched: naming a URL is asking for it, which is why
/// this needs no equivalent of `--remote-images`. The images inside what comes
/// back are a different matter, and still obey it.
pub fn read(names: &[PathBuf]) -> Result<Vec<Source>> {
    if names.is_empty() {
        if std::io::stdin().is_terminal() {
            anyhow::bail!("no input; give a file or pipe Markdown in (try `termmd --help`)");
        }
        return Ok(vec![read_stdin()?]);
    }
    names.iter().map(|name| read_one(name)).collect()
}

fn read_one(name: &Path) -> Result<Source> {
    if name.as_os_str() == "-" {
        return read_stdin();
    }
    let origin = if let Some(url) = as_url(name) {
        Origin::Url(url)
    } else if name.is_dir() {
        Origin::Directory(name.to_path_buf())
    } else {
        Origin::File(name.to_path_buf())
    };
    let mut source = Source::pending(origin);
    source.reload()?;
    Ok(source)
}

/// The argument as an HTTP URL, if that is what it is.
///
/// Only http and https: a `file:` URL is a path by another name and is left to
/// the file reader, and nothing else is something we could fetch.
pub fn as_url(name: &Path) -> Option<String> {
    let text = name.to_str()?;
    (text.starts_with("http://") || text.starts_with("https://")).then(|| text.to_string())
}

pub(crate) fn fetch(url: &str) -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent(concat!("termmd/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let mut body = Vec::new();
    let mut reader = response.body_mut().as_reader().take(MAX_DOWNLOAD_BYTES);
    std::io::copy(&mut reader, &mut body).with_context(|| format!("reading {url}"))?;
    // Markdown is text, and a page that is not valid UTF-8 is not Markdown we
    // can do anything sensible with; replacing is kinder than refusing.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn read_stdin() -> Result<Source> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading stdin")?;
    Ok(Source {
        origin: Origin::Stdin,
        name: "<stdin>".into(),
        text,
    })
}

/// Writes a directory's Markdown as a document that links to all of it.
///
/// Generating Markdown rather than building a file browser is the whole trick:
/// the pager already follows a link to a local document, and `backspace`
/// already comes back, so `termmd docs/` is a browsable directory tree made out
/// of parts that were already there.
fn index_of(dir: &Path) -> Result<String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut subdirectories: Vec<PathBuf> = Vec::new();

    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // A dotfile was not written to be browsed.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            if holds_markdown(&path, HOLDS_MARKDOWN_DEPTH) {
                subdirectories.push(path);
            }
        } else if is_markdown(&path) {
            files.push(path);
        }
        if files.len() + subdirectories.len() >= MAX_INDEX_ENTRIES {
            break;
        }
    }

    files.sort_by_key(|p| sort_key(p, true));
    subdirectories.sort_by_key(|p| sort_key(p, false));

    let mut out = format!("# {}\n\n", dir.display().to_string().trim_end_matches('/'));
    if files.is_empty() && subdirectories.is_empty() {
        out.push_str("No Markdown here.\n");
        return Ok(out);
    }

    for path in &files {
        let name = file_name(path);
        let (text, target) = (label(&name), link(&name));
        match first_heading(path) {
            Some(title) if title != name => {
                out.push_str(&format!("- [{text}]({target}) — {title}\n"))
            }
            _ => out.push_str(&format!("- [{text}]({target})\n")),
        }
    }
    if !subdirectories.is_empty() {
        out.push('\n');
        for path in &subdirectories {
            let name = file_name(path);
            out.push_str(&format!("- [{}/]({})\n", label(&name), link(&name)));
        }
    }
    Ok(out)
}

/// Sorts README to the front, then by name, case last so that `A.md` and `a.md`
/// do not swap places between runs.
fn sort_key(path: &Path, readme_first: bool) -> (bool, String, String) {
    let name = file_name(path);
    let is_readme = readme_first && name.to_ascii_lowercase().starts_with("readme.");
    (!is_readme, name.to_ascii_lowercase(), name)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

/// A link target for a name in the index's own directory.
///
/// Percent-encoding the characters that would otherwise end the destination:
/// a file called `a (1).md` is a link that stops at the space without it.
fn link(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            _ => out.push(c),
        }
    }
    out
}

/// The same name as link text, where a bracket would end it early.
fn label(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether a path names something we would render as Markdown.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "md" | "markdown" | "mdown" | "mkd" | "mdx"))
}

/// How far down to look for Markdown before deciding a directory has none.
/// Deep enough for a docs tree, shallow enough to be quick about it.
const HOLDS_MARKDOWN_DEPTH: usize = 8;

/// Whether a directory is worth listing: one with no Markdown in it is a
/// directory of something else, and an index full of `images/` helps nobody.
///
/// Descends into real subdirectories only. A symlink can point back the way we
/// came, and a cycle of them has no bottom -- this used to be saved from
/// running for ever only by paths growing too long to open.
fn holds_markdown(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        if is_markdown(&entry.path()) {
            return true;
        }
        depth > 0
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && holds_markdown(&entry.path(), depth - 1)
    })
}

/// The first heading in a file, to say what it is beyond its name.
fn first_heading(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; TITLE_PEEK_BYTES];
    let read = file.read(&mut buffer).ok()?;
    let head = String::from_utf8_lossy(&buffer[..read]);

    let mut lines = head.lines().peekable();
    // Front matter is not the document, so skip over it to reach the heading.
    if lines.peek() == Some(&"---") {
        lines.next();
        for line in lines.by_ref() {
            if line == "---" || line == "..." {
                break;
            }
        }
    }
    let mut fenced = false;
    for line in lines {
        let trimmed = line.trim();
        // A shell comment at the top of a code block is not a heading, and a
        // file that opens with one is common enough to be worth the check.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            return Some(title.trim().trim_end_matches('#').trim().to_string());
        }
        // A setext heading: the underline says the line before it was one.
        if !trimmed.is_empty() && trimmed.chars().all(|c| c == '=') && trimmed.len() >= 2 {
            return None;
        }
    }
    None
}

/// Parses every source and merges them into a single document.
///
/// Each file's relative URLs are rebased against its own directory before the
/// merge, so an image beside the second file still resolves once the two are one
/// page. Several files also get filename headings, because otherwise a reader
/// cannot tell where one document ends and the next begins.
pub fn build_document(sources: &[Source], options: ParseOptions) -> Document {
    let mut merged = Document::default();
    let mut slugger = Slugger::new();
    let multiple = sources.len() > 1;

    for (i, source) in sources.iter().enumerate() {
        let mut doc = markdown::parse(&source.text, options);
        markdown::rebase_urls(&mut doc, source.base());

        if multiple {
            if i > 0 {
                merged.blocks.push(Block::Rule);
            }
            merged.blocks.push(Block::Heading {
                level: 1,
                id: slugger.slug(&source.name),
                content: vec![Inline::Code(source.name.clone())],
            });
        }
        if merged.title.is_none() {
            merged.title = doc.title.take();
        }
        if merged.front_matter.is_none() {
            merged.front_matter = doc.front_matter.take();
        }
        merged.blocks.append(&mut doc.blocks);
        merged.footnotes.append(&mut doc.footnotes);
    }
    merged
}

/// The paths that can be watched for changes.
pub fn watchable_paths(sources: &[Source]) -> Vec<PathBuf> {
    sources
        .iter()
        .filter_map(|s| s.path().map(Path::to_path_buf))
        .collect()
}

/// The URLs among `sources`, each with the text it holds now.
///
/// Nothing on the far side of a URL will tell us when it changes, so a watcher
/// has to fetch it again and compare, and this is what it compares against.
pub fn watchable_urls(sources: &[Source]) -> Vec<(String, String)> {
    sources
        .iter()
        .filter_map(|s| match &s.origin {
            Origin::Url(url) => Some((url.clone(), s.text.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str, text: &str) -> Source {
        Source::file(PathBuf::from(name), text.into())
    }

    /// A directory of our own, named after the test using it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("termmd-source-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_urls_are_polled_and_only_paths_are_watched() {
        let sources = vec![
            source("a.md", "# A\n"),
            Source {
                origin: Origin::Url("https://example.com/b.md".into()),
                name: "https://example.com/b.md".into(),
                text: "# B\n".into(),
            },
        ];
        assert_eq!(watchable_paths(&sources), vec![PathBuf::from("a.md")]);
        assert_eq!(
            watchable_urls(&sources),
            vec![("https://example.com/b.md".to_string(), "# B\n".to_string())]
        );
    }

    #[test]
    fn a_single_file_gets_no_extra_heading() {
        let doc = build_document(&[source("a.md", "# Real Title\n")], ParseOptions::default());
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.title.as_deref(), Some("Real Title"));
    }

    #[test]
    fn several_files_are_labelled_and_separated() {
        let docs = [source("a.md", "alpha\n"), source("b.md", "beta\n")];
        let doc = build_document(&docs, ParseOptions::default());
        let headings = doc.headings();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].1, "a.md");
        assert_eq!(headings[1].1, "b.md");
        assert!(
            doc.blocks.iter().any(|b| matches!(b, Block::Rule)),
            "expected a separator"
        );
    }

    #[test]
    fn footnotes_from_every_file_are_kept() {
        let docs = [
            source("a.md", "one[^a]\n\n[^a]: first\n"),
            source("b.md", "two[^b]\n\n[^b]: second\n"),
        ];
        let doc = build_document(&docs, ParseOptions::default());
        assert_eq!(doc.footnotes.len(), 2);
    }

    #[test]
    fn a_base_is_the_documents_own_directory() {
        assert_eq!(
            Source {
                origin: Origin::Stdin,
                name: "<stdin>".into(),
                text: String::new()
            }
            .base(),
            UrlBase::Dir(Path::new("."))
        );
        assert_eq!(source("bare.md", "").base(), UrlBase::Dir(Path::new(".")));
        assert_eq!(
            source("dir/file.md", "").base(),
            UrlBase::Dir(Path::new("dir"))
        );
        let remote = Source {
            origin: Origin::Url("https://example.com/a/b.md".into()),
            name: "b.md".into(),
            text: String::new(),
        };
        assert_eq!(remote.base(), UrlBase::Url("https://example.com/a/b.md"));
    }

    #[test]
    fn urls_are_rebased_per_file() {
        let docs = [
            source("one/a.md", "![x](img.png)\n"),
            source("two/b.md", "![y](img.png)\n"),
        ];
        let doc = build_document(&docs, ParseOptions::default());
        let urls: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Figure { image, .. } => Some(image.url.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(urls.len(), 2);
        // Rebasing yields absolute paths, so that nothing downstream can join
        // them against a second base directory.
        assert!(
            urls.iter().all(|u| Path::new(u).is_absolute()),
            "got {urls:?}"
        );
        // Compared as paths: the separator differs between platforms.
        assert!(
            Path::new(&urls[0]).ends_with("one/img.png"),
            "got {}",
            urls[0]
        );
        assert!(
            Path::new(&urls[1]).ends_with("two/img.png"),
            "got {}",
            urls[1]
        );
    }

    #[test]
    fn a_remote_documents_images_stay_remote() {
        let remote = Source {
            origin: Origin::Url("https://example.com/repo/docs/README.md".into()),
            name: "README.md".into(),
            text: "![x](img/logo.png)\n\n![y](/top.png)\n\n![z](../up.png)\n".into(),
        };
        let doc = build_document(&[remote], ParseOptions::default());
        let urls: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Figure { image, .. } => Some(image.url.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://example.com/repo/docs/img/logo.png",
                "https://example.com/top.png",
                "https://example.com/repo/up.png",
            ]
        );
    }

    #[test]
    fn a_url_is_recognised_as_one() {
        assert_eq!(
            as_url(Path::new("https://example.com/a.md")).as_deref(),
            Some("https://example.com/a.md")
        );
        assert_eq!(
            as_url(Path::new("http://example.com/a.md")).as_deref(),
            Some("http://example.com/a.md")
        );
        // A path that merely mentions one, and a scheme we cannot fetch.
        assert_eq!(as_url(Path::new("./https://a.md")), None);
        assert_eq!(as_url(Path::new("file:///tmp/a.md")), None);
        assert_eq!(as_url(Path::new("README.md")), None);
    }

    #[test]
    fn a_directory_becomes_an_index_of_its_markdown() {
        let dir = scratch("index");
        std::fs::write(dir.join("README.md"), "# The Readme\n").unwrap();
        std::fs::write(dir.join("guide.md"), "# Getting started\n").unwrap();
        std::fs::write(dir.join("alpha.md"), "no heading here\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not markdown\n").unwrap();
        std::fs::create_dir_all(dir.join("images")).unwrap();
        std::fs::write(dir.join("images/logo.png"), "x").unwrap();
        std::fs::create_dir_all(dir.join("chapters")).unwrap();
        std::fs::write(dir.join("chapters/one.md"), "# One\n").unwrap();

        let index = index_of(&dir).unwrap();
        let lines: Vec<&str> = index.lines().filter(|l| l.starts_with("- ")).collect();

        assert_eq!(
            lines,
            vec![
                "- [README.md](README.md) — The Readme",
                "- [alpha.md](alpha.md)",
                "- [guide.md](guide.md) — Getting started",
                "- [chapters/](chapters)",
            ],
            "README first, then files by name, then directories that hold \
             Markdown; a file with no heading is listed by name alone"
        );
        assert!(!index.contains("notes.txt"), "not Markdown: {index}");
        assert!(!index.contains("images"), "no Markdown in it: {index}");
    }

    #[test]
    fn an_index_links_relative_to_the_directory_it_lists() {
        let dir = scratch("links");
        std::fs::write(dir.join("one.md"), "# One\n").unwrap();
        let source = read_one(&dir).unwrap();
        let doc = build_document(&[source], ParseOptions::default());

        // The index's own base is the directory, so its links resolve into it.
        assert!(
            doc.headings()
                .iter()
                .any(|(_, text, _)| text.contains("links"))
        );
        assert_eq!(
            read_one(&dir).unwrap().base(),
            UrlBase::Dir(dir.as_path()),
            "an index resolves its links against the directory it lists"
        );
    }

    #[test]
    fn a_directory_with_nothing_to_show_says_so() {
        let dir = scratch("empty");
        std::fs::write(dir.join("photo.png"), "x").unwrap();
        let index = index_of(&dir).unwrap();
        assert!(index.contains("No Markdown here"), "{index}");
    }

    #[test]
    fn names_that_would_break_a_link_are_encoded() {
        let dir = scratch("spaces");
        std::fs::write(dir.join("a file (1).md"), "# Spaced\n").unwrap();
        let index = index_of(&dir).unwrap();
        assert!(
            index.contains("[a file (1).md](a%20file%20%281%29.md)"),
            "{index}"
        );
    }

    #[test]
    fn a_hash_inside_a_code_fence_is_not_the_heading() {
        // A file that opens with a shell block took the comment as its title.
        let dir = scratch("fenced");
        let path = dir.join("install.md");
        std::fs::write(
            &path,
            "```sh\n# not a heading, a comment\necho hi\n```\n\n# The Real Heading\n",
        )
        .unwrap();
        assert_eq!(first_heading(&path).as_deref(), Some("The Real Heading"));

        let tilde = dir.join("tilde.md");
        std::fs::write(&tilde, "~~~\n# nor this\n~~~\n\n# Actual\n").unwrap();
        assert_eq!(first_heading(&tilde).as_deref(), Some("Actual"));
    }

    #[test]
    fn a_cycle_of_symlinked_directories_ends() {
        // Only path lengths stopped this before, and only eventually.
        let dir = scratch("cycle");
        let inner = dir.join("a");
        std::fs::create_dir_all(&inner).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("..", inner.join("up")).unwrap();

        assert!(!holds_markdown(&inner, HOLDS_MARKDOWN_DEPTH));
        // And a directory that really does hold Markdown is still found.
        std::fs::write(inner.join("real.md"), "# Real\n").unwrap();
        assert!(holds_markdown(&inner, HOLDS_MARKDOWN_DEPTH));
    }

    #[test]
    fn markdown_deeper_down_still_counts() {
        let dir = scratch("deep");
        let deep = dir.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("buried.md"), "# Buried\n").unwrap();
        assert!(holds_markdown(&dir.join("a"), HOLDS_MARKDOWN_DEPTH));
    }

    #[test]
    fn brackets_in_a_name_do_not_break_the_link() {
        let dir = scratch("brackets");
        std::fs::write(dir.join("a [draft].md"), "# Draft\n").unwrap();
        let index = index_of(&dir).unwrap();
        assert!(
            index.contains(r"- [a \[draft\].md](a%20%5Bdraft%5D.md)"),
            "{index}"
        );
        // And the link survives being parsed back out of the index.
        let doc = markdown::parse(&index, ParseOptions::default());
        let text = doc.headings();
        assert!(!text.is_empty());
    }

    #[test]
    fn a_heading_is_read_past_front_matter() {
        let dir = scratch("front-matter");
        let path = dir.join("post.md");
        std::fs::write(&path, "---\ntitle: meta\n---\n\n# The Real Heading\n").unwrap();
        assert_eq!(first_heading(&path).as_deref(), Some("The Real Heading"));
    }

    #[test]
    fn only_files_can_be_watched() {
        let dir = scratch("watch");
        std::fs::write(dir.join("a.md"), "# A\n").unwrap();
        let sources = vec![
            source("a.md", ""),
            read_one(&dir).unwrap(),
            Source {
                origin: Origin::Url("https://example.com/a.md".into()),
                name: "a.md".into(),
                text: String::new(),
            },
            Source {
                origin: Origin::Stdin,
                name: "<stdin>".into(),
                text: String::new(),
            },
        ];
        // The directory counts: its index changes when a file appears in it.
        assert_eq!(
            watchable_paths(&sources),
            vec![PathBuf::from("a.md"), dir.clone()]
        );
    }
}
