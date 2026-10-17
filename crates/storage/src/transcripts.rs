//! Transcript files: raw session output lives on disk, rotated at a
//! size cap. The event log carries spans + previews, not the bytes.

use crate::error::StorageResult;
use orc_domain::{SessionId, Stream};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Rotate a transcript file when it exceeds this size.
const ROTATE_AT: u64 = 8 * 1024 * 1024; // 8 MiB
/// Keep at most this many rotated segments per session+stream.
const KEEP_SEGMENTS: u32 = 8;

#[derive(Clone)]
pub struct TranscriptStore {
    root: PathBuf,
}

/// Where a chunk landed — the span events reference.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

impl TranscriptStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("transcripts"),
        }
    }

    fn file_for(&self, session: &SessionId, stream: Stream) -> PathBuf {
        let name = match stream {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
            Stream::Agent => "agent",
            Stream::System => "system",
        };
        self.root.join(session.as_str()).join(format!("{name}.log"))
    }

    /// Append bytes; rotates when the active file exceeds the cap.
    /// Returns the byte span in the *current* segment.
    pub async fn append(
        &self,
        session: &SessionId,
        stream: Stream,
        bytes: &[u8],
    ) -> StorageResult<Span> {
        let path = self.file_for(session, stream);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let meta = tokio::fs::metadata(&path).await.ok();
        let mut offset = meta.map(|m| m.len()).unwrap_or(0);
        if offset >= ROTATE_AT {
            self.rotate(session, stream).await?;
            offset = 0;
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        f.write_all(bytes).await?;
        f.flush().await?;
        Ok(Span {
            offset,
            len: bytes.len() as u64,
        })
    }

    /// Read a span from the active segment.
    pub async fn read(
        &self,
        session: &SessionId,
        stream: Stream,
        offset: u64,
        len: u64,
    ) -> StorageResult<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let path = self.file_for(session, stream);
        let mut f = tokio::fs::File::open(&path).await?;
        f.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Tail of the transcript (for inspector initial load).
    pub async fn tail(
        &self,
        session: &SessionId,
        stream: Stream,
        max_bytes: u64,
    ) -> StorageResult<(u64, Vec<u8>)> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let path = self.file_for(session, stream);
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((0, Vec::new()));
            }
            Err(e) => return Err(e.into()),
        };
        let size = meta.len();
        let start = size.saturating_sub(max_bytes);
        let mut f = tokio::fs::File::open(&path).await?;
        f.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = Vec::with_capacity((size - start) as usize);
        f.read_to_end(&mut buf).await?;
        Ok((start, buf))
    }

    async fn rotate(&self, session: &SessionId, stream: Stream) -> StorageResult<()> {
        let active = self.file_for(session, stream);
        // Shift segments: .7 deleted, .6→.7, ..., active→.1
        for i in (1..KEEP_SEGMENTS).rev() {
            let from = segment_path(&active, i);
            let to = segment_path(&active, i + 1);
            if from.exists() {
                let _ = tokio::fs::rename(&from, &to).await;
            }
        }
        let _ = tokio::fs::rename(&active, segment_path(&active, 1)).await;
        // Drop the oldest beyond KEEP_SEGMENTS.
        let old = segment_path(&active, KEEP_SEGMENTS + 1);
        if old.exists() {
            let _ = tokio::fs::remove_file(&old).await;
        }
        Ok(())
    }
}

fn segment_path(active: &Path, n: u32) -> PathBuf {
    let mut name = active.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{n}"));
    active.with_file_name(name)
}
