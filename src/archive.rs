use std::fs;
use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::discover::DiskFile;
use crate::store::ArchiveRow;

/// The archive's row for `file`. `None` if it cannot be read: its stamp stays as it was, so the next run tries again.
pub fn row(file: &DiskFile) -> Option<ArchiveRow> {
    let bytes = fs::read(&file.path).ok()?;
    Some(ArchiveRow {
        path: file.key.clone(),
        parent: file.parent.clone(),
        project_dir: file.project_dir.clone(),
        session_id: file.session_id.clone(),
        meta: file.meta.as_ref().and_then(|meta| fs::read_to_string(meta).ok()),
        stamp: file.stamp,
        data: compress(&slim(&bytes))?,
    })
}

pub fn decompress(data: &[u8]) -> Option<String> {
    let mut bytes = Vec::new();
    ZlibDecoder::new(data).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn compress(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

fn slim(transcript: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(transcript.len() / 2);
    for line in transcript.split(|&b| b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(mut entry) = serde_json::from_slice::<serde_json::Value>(line) else {
            out.extend_from_slice(line);
            out.push(b'\n');
            continue;
        };
        if entry.get("type").and_then(serde_json::Value::as_str) == Some("attachment") {
            continue;
        }
        if let Some(object) = entry.as_object_mut() {
            object.remove("toolUseResult");
            object.remove("serverClassifierContext");
        }
        if let Some(blocks) = entry.pointer_mut("/message/content").and_then(serde_json::Value::as_array_mut) {
            for block in blocks.iter_mut().filter_map(serde_json::Value::as_object_mut) {
                block.remove("signature");
                if block.get("type").and_then(serde_json::Value::as_str) == Some("redacted_thinking") {
                    block.remove("data");
                }
            }
        }
        if serde_json::to_writer(&mut out, &entry).is_ok() {
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::discover::FileStamp;

    #[test]
    fn slim_drops_bulk_but_keeps_the_conversation() {
        let input = [
            r#"{"type":"user","timestamp":"t","toolUseResult":{"stdout":"big"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"out"}]}}"#,
            r#"{"type":"attachment","attachment":{"type":"instructions","content":"huge"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm","signature":"c2ln"},{"type":"redacted_thinking","data":"enc"}]}}"#,
            "not json",
        ]
        .join("\n");
        let slimmed = String::from_utf8(slim(input.as_bytes())).unwrap();
        assert!(!slimmed.contains("toolUseResult"));
        assert!(!slimmed.contains("instructions"));
        assert!(!slimmed.contains("c2ln") && !slimmed.contains("enc"));
        assert!(slimmed.contains(r#""content":"out""#) && slimmed.contains(r#""thinking":"hmm""#));
        assert!(slimmed.ends_with("not json\n"));
    }

    #[test]
    fn slim_skips_blank_lines_and_ends_every_line() {
        assert_eq!(slim(b"{\"a\":1}\n\n   \n{\"b\":2}"), b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(slim(b""), b"");
    }

    #[test]
    fn compression_round_trips() {
        let text = "a transcript line\n".repeat(100);
        let compressed = compress(text.as_bytes()).unwrap();
        assert!(compressed.len() < text.len());
        assert_eq!(decompress(&compressed).unwrap(), text);
        assert_eq!(decompress(&compress(b"").unwrap()).unwrap(), "");
    }

    #[test]
    fn invalid_utf8_comes_back_as_replacement_characters() {
        assert_eq!(decompress(&compress(b"caf\xc3\xa9 \xff!").unwrap()).unwrap(), "café \u{fffd}!");
    }

    #[test]
    fn data_that_is_not_compressed_reads_as_nothing() {
        assert_eq!(decompress(b"plain text"), None);
    }

    fn disk_file(path: &Path, meta: Option<&Path>) -> DiskFile {
        DiskFile {
            path: path.to_owned(),
            key: "-p/s1/subagents/a.jsonl".to_owned(),
            project_dir: "-p".to_owned(),
            session_id: "s1".to_owned(),
            parent: Some("-p/s1.jsonl".to_owned()),
            meta: meta.map(Path::to_owned),
            stamp: FileStamp { size: 3, mtime: 9 },
        }
    }

    #[test]
    fn a_row_names_the_file_as_the_index_does_and_holds_it_slimmed_and_compressed() {
        let dir = TempDir::new().unwrap();
        let (path, meta) = (dir.path().join("a.jsonl"), dir.path().join("a.meta.json"));
        fs::write(&path, b"{\"type\":\"attachment\"}\nnot json \xff\n").unwrap();
        fs::write(&meta, "{\"description\":\"d\"}").unwrap();

        let row = row(&disk_file(&path, Some(&meta))).unwrap();
        assert_eq!(row.path, "-p/s1/subagents/a.jsonl");
        assert_eq!(row.parent.as_deref(), Some("-p/s1.jsonl"));
        assert_eq!((row.project_dir.as_str(), row.session_id.as_str()), ("-p", "s1"));
        assert_eq!(row.stamp, FileStamp { size: 3, mtime: 9 });
        assert_eq!(row.meta.as_deref(), Some("{\"description\":\"d\"}"));
        assert_eq!(decompress(&row.data).unwrap(), "not json \u{fffd}\n");
    }

    #[test]
    fn a_missing_meta_file_leaves_the_row_without_meta() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, "{}").unwrap();
        assert_eq!(row(&disk_file(&path, Some(&dir.path().join("gone.json")))).unwrap().meta, None);
    }

    #[test]
    fn a_file_that_cannot_be_read_has_no_row() {
        let dir = TempDir::new().unwrap();
        assert!(row(&disk_file(&dir.path().join("gone.jsonl"), None)).is_none());
    }
}
