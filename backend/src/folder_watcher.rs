use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::mpsc;
use walkdir::WalkDir;

pub struct FileInfo {
    pub path: PathBuf,
    pub file_type: String,
    pub name: String,
}

pub struct FolderWatcher {
    watch_path: PathBuf,
    tx: mpsc::Sender<FileInfo>,
}

impl FolderWatcher {
    pub fn new(watch_path: PathBuf, tx: mpsc::Sender<FileInfo>) -> Self {
        Self { watch_path, tx }
    }

    pub async fn start(self) {
        let (event_tx, mut event_rx) = mpsc::channel(100);
        let watch_path = self.watch_path.clone();
        let tx = self.tx.clone();

        // Scan existing files first
        tokio::spawn(async move {
            if let Err(e) = Self::scan_existing_files(&watch_path, &tx).await {
                eprintln!("Error scanning existing files: {}", e);
            }
        });

        // Setup file watcher
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create Tokio runtime for watcher");

            rt.block_on(async {
                let mut watcher: RecommendedWatcher = match Watcher::new(
                    move |res: Result<Event, notify::Error>| {
                        if let Ok(event) = res {
                            let _ = event_tx.blocking_send(event);
                        }
                    },
                    Config::default().with_poll_interval(Duration::from_secs(2)),
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("Failed to create watcher: {}", e);
                        return;
                    }
                };

                if let Err(e) = watcher.watch(&self.watch_path, RecursiveMode::Recursive) {
                    eprintln!("Failed to watch directory: {}", e);
                    return;
                }

                println!("Watching folder: {:?}", self.watch_path);
                let mut processed_files: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();

                while let Some(event) = event_rx.recv().await {
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            for path in event.paths {
                                if path.is_file() {
                                    // Check if we've already processed this file recently
                                    let should_process = match processed_files.get(&path) {
                                        Some(last_time) => {
                                            if let Ok(elapsed) = last_time.elapsed() {
                                                elapsed.as_secs() > 2
                                            } else {
                                                false
                                            }
                                        }
                                        None => true,
                                    };

                                    if should_process {
                                        if let Some(file_info) = Self::process_file(&path) {
                                            processed_files.insert(
                                                path.clone(),
                                                std::time::SystemTime::now(),
                                            );
                                            if let Err(e) = self.tx.send(file_info).await {
                                                eprintln!("Failed to send file info: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Keep watcher alive
                drop(watcher);
            });
        });
    }

    async fn scan_existing_files(
        path: &Path,
        tx: &mpsc::Sender<FileInfo>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!("Scanning existing files in: {:?}", path);
        let mut count = 0;

        for entry in WalkDir::new(path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().is_file() {
                if let Some(file_info) = Self::process_file(entry.path()) {
                    tx.send(file_info).await?;
                    count += 1;
                }
            }
        }

        println!("Found {} existing model files", count);
        Ok(())
    }

    fn process_file(path: &Path) -> Option<FileInfo> {
        let extension = path.extension()?.to_str()?;
        let file_type = match extension.to_lowercase().as_str() {
            "gltf" => "gltf",
            "glb" => "glb",
            "json" => {
                // Check if it's a Houdini JSON by reading the file
                if let Ok(content) = std::fs::read_to_string(path) {
                    if Self::is_houdini_json_content(&content) {
                        "houdini_json"
                    } else {
                        return None; // Skip non-Houdini JSON files
                    }
                } else {
                    return None;
                }
            }
            _ => return None,
        };

        let name = path
            .file_stem()?
            .to_str()?
            .to_string();

        Some(FileInfo {
            path: path.to_path_buf(),
            file_type: file_type.to_string(),
            name,
        })
    }

    fn is_houdini_json_content(content: &str) -> bool {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
            if let Some(array) = json.as_array() {
                for item in array {
                    if let Some(key) = item.as_str() {
                        match key {
                            "fileversion" | "pointcount" | "vertexcount" | "primitivecount" => {
                                return true
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        false
    }
}