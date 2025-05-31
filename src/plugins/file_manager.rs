use log::debug;
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::{delete, post, put};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use tokio::fs::{read_link, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

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

#[derive(Debug, Default)]
pub struct FileManagerPlugin {
    problematic_paths: RwLock<Vec<PathBuf>>,
}
impl FileManagerPlugin {
    pub fn new() -> FileManagerPlugin {
        FileManagerPlugin::default()
    }
    pub async fn list(&self, path: Option<&Path>) -> Result<Vec<FileEntry>, Error> {
        let user_path = path.unwrap_or(Path::new("/")).to_path_buf();
        let path = if user_path.starts_with("~") {
            let stripped = user_path
                .strip_prefix("~")
                .expect("Checked Path Existence Above");
            if let Some(home_dir) = home::home_dir() {
                home_dir.join(stripped)
            } else {
                stripped.to_path_buf()
            }
        } else {
            user_path
        };
        if self.problematic_paths.read().await.contains(&path) {
            return Err(Error::new(ErrorKind::InvalidInput, "Path is not valid"));
        } else if is_fuse_filesystem(&path.to_string_lossy()).await? {
            self.problematic_paths.write().await.push(path.clone());
            return Err(Error::new(ErrorKind::InvalidInput, "Path is not valid"));
        }
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut dir_entry = tokio::fs::read_dir(path).await?;
        while let Some(entry) = dir_entry.next_entry().await? {
            let file_type = entry.file_type().await?;
            let path_buf = entry.path();
            let path = path_buf.to_string_lossy().to_string();
            debug!("Found entry: {}", path);
            let (entry_type, size) = if file_type.is_dir() {
                if self.problematic_paths.read().await.contains(&path_buf) {
                    continue;
                } else if is_fuse_filesystem(&path).await? {
                    self.problematic_paths.write().await.push(path_buf.clone());
                    continue;
                } else {
                    (EntryType::Directory, 0)
                }
            } else if file_type.is_file() {
                (EntryType::File, entry.metadata().await?.len())
            } else if file_type.is_symlink() {
                let linked_path = read_link(entry.path()).await?;
                (EntryType::SymLink(linked_path), 0)
            } else {
                (EntryType::Unknown, 0)
            };
            debug!("Entry Type: {:?}", entry_type);
            entries.push(FileEntry {
                path,
                entry_type,
                size,
            })
        }
        Ok(entries)
    }
    pub async fn get_file_content<P: AsRef<Path>>(&self, path: P) -> Result<FileContents, Error> {
        let file = tokio::fs::File::open(path.as_ref()).await?;
        let meta_data = file.metadata().await?;
        if meta_data.is_dir() {
            Err(Error::new(
                ErrorKind::IsADirectory,
                "Cannot open Directory as File",
            ))
        } else {
            let contents = tokio::fs::read_to_string(path).await?;
            let mime_type = infer::get(contents.as_bytes())
                .map(|v| v.mime_type().to_string())
                .unwrap_or("Unknown".to_string());
            Ok(FileContents {
                contents,
                mime_type,
            })
        }
    }
    pub async fn create_file<P: AsRef<Path>>(
        &self,
        path: P,
        contents: &[u8],
    ) -> Result<bool, Error> {
        let mut file = tokio::fs::File::create_new(path.as_ref()).await?;
        file.write_all(contents).await?;
        Ok(true)
    }
    pub async fn update<P: AsRef<Path>>(&self, path: P, contents: &[u8]) -> Result<bool, Error> {
        let path = path.as_ref().to_path_buf();
        let contents = contents.to_vec();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut tmp_file = match path.parent() {
                Some(parent) => NamedTempFile::new_in(parent),
                None => NamedTempFile::new(),
            }?;
            tmp_file.write_all(&contents)?;
            tmp_file.persist(path)?;
            Ok(true)
        })
        .await?
    }
    pub async fn create_directory<P: AsRef<Path>>(&self, path: P) -> Result<bool, Error> {
        tokio::fs::create_dir_all(path.as_ref()).await?;
        Ok(true)
    }
    pub async fn rename<P: AsRef<Path>, T: AsRef<Path>>(
        &self,
        from: P,
        to: T,
    ) -> Result<bool, Error> {
        tokio::fs::rename(from, to).await?;
        Ok(true)
    }
    pub async fn remove<P: AsRef<Path>>(&self, path: P) -> Result<bool, Error> {
        if path.as_ref().is_dir() {
            tokio::fs::remove_dir(path).await?;
        } else {
            tokio::fs::remove_file(path).await?;
        }
        Ok(true)
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

pub async fn is_fuse_filesystem(path: &str) -> Result<bool, Error> {
    const MOUNTS_FILE: &str = "/proc/mounts";
    let file = File::open(MOUNTS_FILE).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[1] == path && parts[2] == "fuse" {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tokio::test]
async fn test_fuse_detect() {
    let is_fuse_path = is_fuse_filesystem("/keybase").await;
    println!("{:?}", is_fuse_path);
}
