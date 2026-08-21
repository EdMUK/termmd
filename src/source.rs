//! Reading input documents, and merging several of them into one page.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::markdown::{self, Block, Document, Inline, ParseOptions, Slugger};

/// One input document, with enough context to re-read it later.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// `None` for stdin, which cannot be re-read.
    pub path: Option<PathBuf>,
    pub name: String,
    pub text: String,
}

impl Source {
    /// The directory relative URLs in this document resolve against.
    pub fn base_dir(&self) -> PathBuf {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Re-reads the file from disk. Sources without a path are left alone.
    pub fn reload(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        self.text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(())
    }
}

/// Reads the files named on the command line, or stdin when there are none.
pub fn read(files: &[PathBuf]) -> Result<Vec<Source>> {
    if files.is_empty() {
        if std::io::stdin().is_terminal() {
            anyhow::bail!("no input; give a file or pipe Markdown in (try `termmd --help`)");
        }
        return Ok(vec![read_stdin()?]);
    }
    files
        .iter()
        .map(|path| {
            if path.as_os_str() == "-" {
                return read_stdin();
            }
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            Ok(Source {
                name: path.display().to_string(),
                path: Some(path.clone()),
                text,
            })
        })
        .collect()
}

fn read_stdin() -> Result<Source> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading stdin")?;
    Ok(Source {
        path: None,
        name: "<stdin>".into(),
        text,
    })
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
        markdown::rebase_urls(&mut doc, &source.base_dir());

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
    sources.iter().filter_map(|s| s.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str, text: &str) -> Source {
        Source {
            path: Some(PathBuf::from(name)),
            name: name.into(),
            text: text.into(),
        }
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
    fn base_dir_falls_back_to_the_current_directory() {
        let s = Source {
            path: None,
            name: "<stdin>".into(),
            text: String::new(),
        };
        assert_eq!(s.base_dir(), PathBuf::from("."));
        assert_eq!(source("bare.md", "").base_dir(), PathBuf::from("."));
        assert_eq!(source("dir/file.md", "").base_dir(), PathBuf::from("dir"));
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
        assert!(urls[0].ends_with("one/img.png"), "got {}", urls[0]);
        assert!(urls[1].ends_with("two/img.png"), "got {}", urls[1]);
    }
}
