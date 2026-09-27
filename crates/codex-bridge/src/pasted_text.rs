use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use codex_bridge::ComposerAttachment;

pub(crate) const MAX_PASTED_TEXT_BYTES: usize = 2 * 1024 * 1024;
const FILE_NAME: &str = "pasted-text.txt";
const FILE_HEADING: &str = "## Pasted text.txt: ";
const REQUEST_HEADING: &str = "## My request:\n";

pub(crate) struct PastedTextWrapper<'a> {
    pub paths: Vec<&'a str>,
    pub request: &'a str,
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn materialize(
    codex_home: &Path,
    prompt: &str,
    attachments: &[ComposerAttachment],
) -> Result<String> {
    let pasted = attachments
        .iter()
        .filter_map(|attachment| match attachment {
            ComposerAttachment::PastedText { id, text } => Some((id.as_str(), text.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if pasted.is_empty() {
        return Ok(prompt.to_owned());
    }
    if pasted.iter().any(|(id, text)| {
        !valid_id(id) || text.trim().is_empty() || text.len() > MAX_PASTED_TEXT_BYTES
    }) {
        bail!("invalid pasted-text attachment");
    }

    let root = codex_home.join("attachments");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    let mut paths = Vec::with_capacity(pasted.len());
    for (id, text) in pasted {
        let directory = root.join(format!("bridge-{id}"));
        match DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&directory)?;
                if !metadata.file_type().is_dir() {
                    bail!("pasted-text attachment directory is not a regular directory");
                }
            }
            Err(error) => return Err(error).context("failed to create pasted-text directory"),
        }
        let path = directory.join(FILE_NAME);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => file
                .write_all(text.as_bytes())
                .context("failed to write pasted-text attachment")?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&path)?.file_type().is_file() {
                    bail!("pasted-text attachment is not a regular file");
                }
                if fs::read(&path).context("failed to read existing pasted text")?
                    != text.as_bytes()
                {
                    bail!("pasted-text attachment id was reused with different content");
                }
            }
            Err(error) => return Err(error).context("failed to create pasted-text attachment"),
        }
        paths.push(path);
    }

    let mut wrapped = String::from("# Files mentioned by the user:\n\n");
    for path in paths {
        wrapped.push_str(FILE_HEADING);
        wrapped.push_str(&path.to_string_lossy());
        wrapped.push_str("\n\n");
    }
    wrapped.push_str(REQUEST_HEADING);
    wrapped.push_str(prompt);
    Ok(wrapped)
}

pub(crate) fn parse_wrapper(text: &str) -> Option<PastedTextWrapper<'_>> {
    let (files, request) = text.split_once(REQUEST_HEADING)?;
    if !files.starts_with("# Files mentioned by the user:\n") {
        return None;
    }
    let paths = files
        .lines()
        .filter_map(|line| line.strip_prefix(FILE_HEADING))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(PastedTextWrapper { paths, request })
}

pub(crate) fn authorized_path(codex_home: &Path, requested: &str) -> Option<PathBuf> {
    let root = fs::canonicalize(codex_home.join("attachments")).ok()?;
    let path = fs::canonicalize(requested).ok()?;
    let relative = path.strip_prefix(&root).ok()?;
    let mut parts = relative.components();
    let directory = parts.next()?.as_os_str().to_str()?;
    let file = parts.next()?.as_os_str();
    if parts.next().is_some()
        || !directory.starts_with("bridge-")
        || !valid_id(&directory[7..])
        || file != FILE_NAME
    {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_is_a_private_file_and_history_wrapper() {
        let root = std::env::temp_dir().join(format!("codex-bridge-paste-{}", std::process::id()));
        let attachment = ComposerAttachment::PastedText {
            id: "d28b8e6c-2ab3-4df6-8d3d-40cb925665d9".into(),
            text: "first line\nsecond line".into(),
        };
        let wrapped = materialize(&root, "Review this", &[attachment.clone()]).unwrap();
        let parsed = parse_wrapper(&wrapped).unwrap();
        assert_eq!(parsed.request, "Review this");
        assert_eq!(parsed.paths.len(), 1);
        let path = Path::new(parsed.paths[0]);
        assert_eq!(fs::read_to_string(path).unwrap(), "first line\nsecond line");
        assert!(!wrapped.contains("first line"));
        assert_eq!(
            authorized_path(&root, parsed.paths[0]),
            Some(fs::canonicalize(path).unwrap())
        );
        assert_eq!(
            materialize(&root, "Review this", &[attachment]).unwrap(),
            wrapped
        );
        fs::remove_dir_all(root).unwrap();
    }
}
