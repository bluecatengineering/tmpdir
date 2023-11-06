#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![allow(clippy::cognitive_complexity)]
//! # TmpDir
//!
//! Useful to create temp directories and copying their contents on completion
//! of some action. Tmp dirs will be created using [`env::temp_dir`] with
//! some random characters prefixed to prevent a name clash
//!
//! `copy` will traverse recursively through a directory and copy all file
//! contents to some destination dir. It will not follow symlinks.
//!
//! ## Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error + Send + 'static>> {
//! use tmpdir::TmpDir;
//! use tokio::{fs, io::AsyncWriteExt};
//! # let tmp = TmpDir::new("foo").await.unwrap();
//!
//! # let tmp_dir = tmp.as_ref().to_path_buf();
//! # fs::create_dir(tmp_dir.clone().join("dir1")).await.unwrap();
//! # let mut file = fs::File::create(tmp_dir.clone().join("dir1").join("file1"))
//! #    .await
//! #    .unwrap();
//! # file.write_all(b"foo").await.unwrap();
//! # let mut file = fs::File::create(tmp_dir.clone().join("file2"))
//! #    .await
//! #    .unwrap();
//! # file.write_all(b"foo").await.unwrap();
//! let new_tmp = TmpDir::new("bar").await.unwrap();
//! new_tmp.copy(tmp.as_ref()).await;
//! new_tmp.close().await; // not necessary to explicitly call
//! # tmp.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! [`env::temp_dir`]: std::env::temp_dir

use futures::stream::{self, Stream, StreamExt};
use rand::{distributions::Alphanumeric, Rng};
use tokio::fs::{self, DirEntry};

use std::{
    env, fmt, io,
    path::{Path, PathBuf},
};

/// A temporary directory with a randomly generated prefix that will
/// automatically delete it's contents on drop
#[derive(Debug)]
pub struct TmpDir {
    inner: PathBuf,
}

const LEN_RNG: usize = 10;

impl TmpDir {
    /// create a new temp dir in `env::temp_dir` with a prefix. ex. `/tmp/prefix-<random chars>`
    pub async fn new(prefix: impl AsRef<str>) -> io::Result<Self> {
        let mut inner = env::temp_dir();
        let s: String = {
            // shrink scope of rng
            let rng = rand::thread_rng();
            rng.sample_iter(Alphanumeric)
                .map(char::from)
                .take(LEN_RNG)
                .collect()
        };

        inner.push(&format!("{}-{}", prefix.as_ref(), s));

        fs::create_dir(&inner).await?;
        Ok(Self { inner })
    }

    /// return inner path as `PathBuf`
    pub fn to_path_buf(&self) -> PathBuf {
        self.as_ref().to_owned()
    }

    /// list the contents of a directory, if we encounter another dir
    /// push to our traversal list
    async fn list_contents(
        path: PathBuf,
        to_visit: &mut Vec<PathBuf>,
    ) -> io::Result<Vec<DirEntry>> {
        let mut dir = fs::read_dir(path).await?;
        let mut files = Vec::new();

        while let Some(child) = dir.next_entry().await? {
            if child.metadata().await?.is_dir() {
                to_visit.push(child.path());
                files.push(child);
            } else {
                files.push(child)
            }
        }

        Ok(files)
    }

    /// returns a stream of `DirEntry` representing the flattened list of
    /// files that are traversable from the entry path
    fn traverse(
        path: impl Into<PathBuf>,
    ) -> impl Stream<Item = io::Result<DirEntry>> + Send + 'static {
        stream::unfold(vec![path.into()], |mut to_visit| async {
            let path = to_visit.pop()?;
            let file_stream = match TmpDir::list_contents(path, &mut to_visit).await {
                Ok(files) => stream::iter(files).map(Ok).left_stream(),
                Err(e) => stream::once(async { Err(e) }).right_stream(),
            };

            Some((file_stream, to_visit))
        })
        .flatten()
    }

    /// recursively copies contents from tmp dir to another
    pub async fn copy(&self, dest_dir: impl AsRef<Path>) -> io::Result<()> {
        // create dest dir if it doesn't exist
        fs::create_dir_all(dest_dir.as_ref()).await?;

        let files = TmpDir::traverse(self.inner.clone());
        tokio::pin!(files);

        while let Some(file) = files.next().await {
            let file = file?;
            let base_path = self.inner.to_path_buf();
            let file_path = file.path();

            // get common base path
            let diff = file_path
                .strip_prefix(&base_path)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dir"))?;

            let dest = dest_dir.as_ref().to_path_buf().join(diff);

            if file.metadata().await?.is_dir() {
                fs::create_dir_all(dest).await?;
            } else {
                fs::copy(file.path(), dest).await?;
            }
        }
        Ok(())
    }

    /// close the tmp dir and nuke it's contents
    pub async fn close(&self) -> io::Result<()> {
        fs::remove_dir_all(&self.inner).await
    }
}

/// deletes itself after dropped
impl Drop for TmpDir {
    fn drop(&mut self) {
        let path = self.inner.clone();
        tokio::spawn(async move {
            let _ = fs::remove_dir_all(path).await;
        });
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        self.inner.as_ref()
    }
}

impl fmt::Display for TmpDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TmpDir {{ path: {:#?} }}", self.inner.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::AsyncWriteExt,
        time::{self, Duration},
    };

    #[tokio::test]
    async fn test_tmp_create() {
        let tmp = TmpDir::new("foo").await.unwrap();
        let metadata = fs::metadata(tmp.as_ref()).await;
        // file exists
        assert!(metadata.is_ok());

        tmp.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_tmp_drop() {
        let path: PathBuf;
        {
            let tmp = TmpDir::new("foo").await.unwrap();
            path = tmp.as_ref().to_owned();
            let metadata = fs::metadata(tmp.as_ref()).await;
            // file exists
            assert!(metadata.is_ok());
            drop(tmp);
        }
        time::sleep(Duration::from_secs(1)).await;
        let result = fs::metadata(&path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tmp_copy() {
        let tmp = TmpDir::new("foo").await.unwrap();
        let metadata = fs::metadata(tmp.as_ref()).await;
        assert!(metadata.is_ok());

        let tmp_dir = tmp.as_ref().to_path_buf();
        fs::create_dir(tmp_dir.clone().join("dir1")).await.unwrap();
        let mut file = fs::File::create(tmp_dir.clone().join("dir1").join("file1"))
            .await
            .unwrap();
        file.write_all(b"foo").await.unwrap();
        let mut file = fs::File::create(tmp_dir.clone().join("file2"))
            .await
            .unwrap();
        file.write_all(b"foo").await.unwrap();
        // file exists

        let tmp2 = TmpDir::new("bar").await.unwrap();

        tmp.copy(tmp2.as_ref()).await.unwrap();
        assert!(
            fs::metadata(tmp2.as_ref().to_path_buf().join("dir1").join("file1"))
                .await
                .is_ok()
        );

        tmp.close().await.unwrap();
        tmp2.close().await.unwrap();
    }
}
