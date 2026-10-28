use super::confined_files::{ConfinedFiles, MAX_FILE_BYTES};
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::{delete, post, put};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub enum EntryType {
    Directory,
    File,
    SymLink(PathBuf),
    Unknown,
}
#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub entry_type: EntryType,
    pub size: u64,
}
#[derive(Serialize)]
pub struct FileContents {
    pub contents: String,
    pub mime_type: String,
}
#[derive(Debug)]
pub struct FileManagerPlugin {
    files: ConfinedFiles,
}
impl Default for FileManagerPlugin {
    fn default() -> Self {
        Self {
            files: ConfinedFiles::new(PathBuf::from("/mnt")),
        }
    }
}
impl FileManagerPlugin {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn list(&self, path: Option<&Path>) -> Result<Vec<FileEntry>, Error> {
        let files = self.files.clone();
        let path = path.unwrap_or(files.root()).to_path_buf();
        tokio::task::spawn_blocking(move || {
            let directory = files.directory(&path)?;
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
                if entries.len() >= 10000 {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Directory has too many entries",
                    ));
                }
                let entry = entry?;
                let kind = entry.file_type()?;
                let entry_type = if kind.is_dir() {
                    EntryType::Directory
                } else if kind.is_file() {
                    EntryType::File
                } else {
                    EntryType::Unknown
                };
                entries.push(FileEntry {
                    path: path.join(entry.file_name()).to_string_lossy().into_owned(),
                    entry_type,
                    size: if kind.is_file() {
                        entry.metadata()?.len()
                    } else {
                        0
                    },
                });
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(entries)
        })
        .await?
    }
    pub async fn get_file_content<P: AsRef<Path>>(&self, path: P) -> Result<FileContents, Error> {
        let files = self.files.clone();
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let contents = files.read(&path)?;
            Ok(FileContents {
                contents,
                mime_type: "text/plain; charset=utf-8".into(),
            })
        })
        .await?
    }
    pub async fn create_file<P: AsRef<Path>>(
        &self,
        path: P,
        contents: &[u8],
    ) -> Result<bool, Error> {
        let files = self.files.clone();
        let path = path.as_ref().to_path_buf();
        if contents.len() > MAX_FILE_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Text files are limited to 1 MiB",
            ));
        }
        let bytes = contents.to_vec();
        tokio::task::spawn_blocking(move || {
            files.create(&path, &bytes)?;
            Ok(true)
        })
        .await?
    }
    pub async fn update<P: AsRef<Path>>(&self, path: P, contents: &[u8]) -> Result<bool, Error> {
        let files = self.files.clone();
        let path = path.as_ref().to_path_buf();
        if contents.len() > MAX_FILE_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Text files are limited to 1 MiB",
            ));
        }
        let bytes = contents.to_vec();
        tokio::task::spawn_blocking(move || {
            files.update(&path, &bytes)?;
            Ok(true)
        })
        .await?
    }
    pub async fn create_directory<P: AsRef<Path>>(&self, path: P) -> Result<bool, Error> {
        let files = self.files.clone();
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            files.mkdir(&path)?;
            Ok(true)
        })
        .await?
    }
    pub async fn rename<P: AsRef<Path>, T: AsRef<Path>>(
        &self,
        from: P,
        to: T,
    ) -> Result<bool, Error> {
        let files = self.files.clone();
        let from = from.as_ref().to_path_buf();
        let to = to.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            files.rename(&from, &to)?;
            Ok(true)
        })
        .await?
    }
    pub async fn remove<P: AsRef<Path>>(&self, path: P) -> Result<bool, Error> {
        let files = self.files.clone();
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            files.remove(&path)?;
            Ok(true)
        })
        .await?
    }
}

#[derive(Deserialize)]
pub struct ListParams {
    path: String,
}

#[post("/api/files/list", output = "json", eoutput = "bytes")]
pub async fn list_files(
    state: State<FileManagerPlugin>,
    params: Json<Option<ListParams>>,
) -> Result<Vec<FileEntry>, Error> {
    match params.inner() {
        Some(params) => state.0.list(Some(Path::new(&params.path))).await,
        None => state.0.list(None).await,
    }
}

#[derive(Deserialize)]
pub struct FileParams {
    path: String,
}

#[post("/api/files/file", output = "json", eoutput = "bytes")]
pub async fn get_file(
    state: State<FileManagerPlugin>,
    params: Json<Option<FileParams>>,
) -> Result<FileContents, Error> {
    match params.inner() {
        Some(params) => state.0.get_file_content(&params.path).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}

#[derive(Deserialize)]
pub struct FileContentsParams {
    path: String,
    contents: Vec<u8>,
}

#[post("/api/files/file/create", output = "json", eoutput = "bytes")]
pub async fn create_file(
    state: State<FileManagerPlugin>,
    params: Json<Option<FileContentsParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.create_file(params.path, &params.contents).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}

#[put("/api/files/file/save", output = "json", eoutput = "bytes")]
pub async fn update_file(
    state: State<FileManagerPlugin>,
    params: Json<Option<FileContentsParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.update(params.path, &params.contents).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}

#[derive(Deserialize)]
pub struct DirectoryCreateParams {
    path: String,
}

#[post("/api/files/directory", output = "json", eoutput = "bytes")]
pub async fn create_directory(
    state: State<FileManagerPlugin>,
    params: Json<Option<DirectoryCreateParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.create_directory(params.path).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}

#[derive(Deserialize)]
pub struct RenameParams {
    from: String,
    to: String,
}

#[post("/api/files/rename", output = "json", eoutput = "bytes")]
pub async fn rename(
    state: State<FileManagerPlugin>,
    params: Json<Option<RenameParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.rename(params.from, params.to).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}

#[derive(Deserialize)]
pub struct DeleteParams {
    path: String,
}

#[delete("/api/files/remove", output = "json", eoutput = "bytes")]
pub async fn remove(
    state: State<FileManagerPlugin>,
    params: Json<Option<DeleteParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.remove(params.path).await,
        None => Err(Error::new(ErrorKind::InvalidInput, "No Path Specified")),
    }
}
