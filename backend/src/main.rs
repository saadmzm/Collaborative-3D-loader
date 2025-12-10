use futures_util::{ SinkExt, StreamExt };
use rusqlite::{ params, Connection, Result };
use serde::{ Deserialize, Serialize };
use std::{
    collections::HashSet,
    path::PathBuf,
    time::Duration,
    env,
};
use tokio::{
    net::{ TcpListener, TcpStream },
    sync::broadcast::{ self, Sender },
};
use tokio_tungstenite::{ accept_async_with_config, tungstenite::Message };
use base64::{ Engine as _, engine::general_purpose };

mod houdini_json;
use houdini_json::HoudiniJsonParser;

mod folder_watcher;
use folder_watcher::FolderWatcher;

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
struct AdditionalFile {
    filename: String,
    data: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
struct ModelListItem {
    id: i32,
    name: Option<String>,
    file_type: String,
}

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    name: Option<String>,
    model_data: Option<String>,
    file_type: Option<String>,
    additional_files: Option<Vec<AdditionalFile>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ModelResponse {
    id: i32,
    name: Option<String>,
    model_data: String,
    file_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_files: Option<Vec<AdditionalFile>>,
}

#[derive(Debug)]
struct ModelData {
    id: i32,
    name: Option<String>,
    model_data: Vec<u8>,
    file_type: String,
    additional_files: Option<String>,
}

#[tokio::main]
async fn main() {
    let watch_folder = env::var("MODEL_WATCH_FOLDER")
        .unwrap_or_else(|_| "frontend/assets".to_string());
    
    let watch_path = PathBuf::from(&watch_folder);
    
    // Create watched folder if it doesn't exist
    if !watch_path.exists() {
        if let Err(e) = std::fs::create_dir_all(&watch_path) {
            eprintln!("Failed to create watch folder: {}", e);
        } else {
            println!("Created watch folder: {:?}", watch_path);
        }
    }

    let listener = TcpListener::bind("127.0.0.1:8000").await.expect("Failed to bind");
    println!("Backend WebSocket server running on ws://127.0.0.1:8000/ws");
    println!("Supported formats: GLTF (.gltf) and Houdini JSON (.json)");
    println!("GLTF files can include external .bin and texture files");
    println!("Watching folder for new models: {:?}", watch_path);

    let (tx, _) = broadcast::channel(16);

    // Setup folder watcher
    let (file_tx, mut file_rx) = tokio::sync::mpsc::channel(100);
    let watcher = FolderWatcher::new(watch_path.clone(), file_tx);
    watcher.start().await;

    // Handle incoming files from watcher
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        while let Some(file_info) = file_rx.recv().await {
            println!("New file detected: {:?} (type: {})", file_info.path, file_info.file_type);
            
            match std::fs::read(&file_info.path) {
                Ok(data) => {
                    let mut additional_files = Vec::new();
                    
                    // If it's a GLTF file, look for related files
                    if file_info.file_type == "gltf" {
                        if let Ok(gltf_text) = std::str::from_utf8(&data) {
                            if let Ok(gltf_json) = serde_json::from_str::<serde_json::Value>(gltf_text) {
                                if let Some(parent_dir) = file_info.path.parent() {
                                    // Find .bin files
                                    if let Some(buffers) = gltf_json.get("buffers").and_then(|b| b.as_array()) {
                                        for buffer in buffers {
                                            if let Some(uri) = buffer.get("uri").and_then(|u| u.as_str()) {
                                                if !uri.starts_with("data:") {
                                                    let bin_path = parent_dir.join(uri);
                                                    if let Ok(bin_data) = std::fs::read(&bin_path) {
                                                        additional_files.push(AdditionalFile {
                                                            filename: uri.to_string(),
                                                            data: general_purpose::STANDARD.encode(&bin_data),
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    
                                    // Find texture files
                                    if let Some(images) = gltf_json.get("images").and_then(|i| i.as_array()) {
                                        for image in images {
                                            if let Some(uri) = image.get("uri").and_then(|u| u.as_str()) {
                                                if !uri.starts_with("data:") {
                                                    let img_path = parent_dir.join(uri);
                                                    if let Ok(img_data) = std::fs::read(&img_path) {
                                                        additional_files.push(AdditionalFile {
                                                            filename: uri.to_string(),
                                                            data: general_purpose::STANDARD.encode(&img_data),
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    
                    // Process the file
                    let final_data = if file_info.file_type == "houdini_json" {
                        match process_houdini_json(&data) {
                            Ok(gltf_data) => gltf_data,
                            Err(e) => {
                                eprintln!("Failed to process Houdini JSON: {}", e);
                                continue;
                            }
                        }
                    } else {
                        data
                    };
                    
                    let additional_files_json = if additional_files.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&additional_files).ok()
                    };
                    
                    // Check if model with this name already exists
                    match check_model_exists_by_name(&file_info.name) {
                        Ok(true) => {
                            println!("Model '{}' already exists in database, skipping", file_info.name);
                            continue;
                        }
                        Ok(false) => {
                            // Model doesn't exist, proceed with insert
                        }
                        Err(e) => {
                            eprintln!("Error checking model existence: {}", e);
                            continue;
                        }
                    }
                    
                    // Insert into database
                    match insert_model(
                        &final_data,
                        Some(&file_info.name),
                        &file_info.file_type,
                        additional_files_json.as_deref(),
                    ) {
                        Ok(new_id) => {
                            println!("Model '{}' added to database with ID: {}", file_info.name, new_id);
                            
                            // Broadcast update with new model ID marker
                            match load_all_models_metadata() {
                                Ok(models) => {
                                    // Create a special message that includes the new model ID
                                    let update_msg = serde_json::json!({
                                        "models": models,
                                        "new_model_id": new_id
                                    });
                                    let update = serde_json::to_string(&update_msg).unwrap();
                                    if let Err(e) = tx_clone.send(update) {
                                        eprintln!("Broadcast error: {}", e);
                                    }
                                }
                                Err(e) => eprintln!("Failed to load models after auto-insert: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Failed to insert model from watched folder: {}", e),
                    }
                }
                Err(e) => eprintln!("Failed to read file {:?}: {}", file_info.path, e),
            }
        }
    });

    // Existing model list polling
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut last_models: HashSet<ModelListItem> = HashSet::new();
        loop {
            match load_all_models_metadata() {
                Ok(models) => {
                    let current_models: HashSet<ModelListItem> = models.into_iter().collect();
                    if current_models != last_models {
                        let updated_list: Vec<ModelListItem> = current_models.iter().cloned().collect();
                        let update = serde_json::to_string(&updated_list).unwrap();
                        if let Err(e) = tx_clone.send(update) {
                            eprintln!("Broadcast error: {}", e);
                        }
                        last_models = current_models;
                    }
                }
                Err(e) => eprintln!("Failed to poll models: {}", e),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    while let Ok((stream, _addr)) = listener.accept().await {
        let tx = tx.clone();
        tokio::spawn(handle_connection(stream, tx));
    }
}

async fn handle_connection(stream: TcpStream, tx: Sender<String>) {
    let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(100 * 1024 * 1024);
    config.max_frame_size = Some(100 * 1024 * 1024);
    config.accept_unmasked_frames = false;
    let ws_stream = match accept_async_with_config(stream, Some(config)).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Failed to accept WebSocket connection: {:?}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let mut rx = tx.subscribe();

    loop {
        tokio::select! {
            Some(Ok(message)) = read.next() => {
                if let Message::Text(text) = message {
                    match serde_json::from_str::<ModelRequest>(&text) {
                        Ok(request) => {
                            match request.action.as_str() {
                                "get_by_id" => {
                                    if let Some(id) = request.id {
                                        match load_model_by_id(id) {
                                            Ok(model) => {
                                                let additional_files = model.additional_files.as_ref().and_then(|json_str| {
                                                    serde_json::from_str::<Vec<AdditionalFile>>(json_str).ok()
                                                });
                                                let response = ModelResponse {
                                                    id: model.id,
                                                    name: model.name,
                                                    model_data: general_purpose::STANDARD.encode(&model.model_data),
                                                    file_type: model.file_type,
                                                    additional_files,
                                                };
                                                let response_str = serde_json::to_string(&response).unwrap();
                                                if let Err(e) = write
                                                    .send(Message::Text(response_str.into()))
                                                    .await
                                                {
                                                    eprintln!("Send error: {:?}", e);
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                send_error(&mut write, &format!("Model not found: {}", e)).await;
                                            }
                                        }
                                    }
                                }
                                "get_all" => {
                                    match load_all_models_metadata() {
                                        Ok(models) => {
                                            let response_str = serde_json::to_string(&models).unwrap();
                                            if let Err(e) = write
                                                .send(Message::Text(response_str.into()))
                                                .await
                                            {
                                                eprintln!("Send error: {:?}", e);
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            send_error(&mut write, &format!("Failed to load models: {}", e)).await;
                                        }
                                    }
                                }
                                "insert" => {
                                    if let Some(base64_data) = request.model_data {
                                        match general_purpose::STANDARD.decode(&base64_data) {
                                            Ok(raw_data) => {
                                                let file_type = request.file_type.unwrap_or_else(|| {
                                                    detect_file_type(&raw_data)
                                                });
                                                
                                                let final_data = if file_type == "houdini_json" {
                                                    match process_houdini_json(&raw_data) {
                                                        Ok(gltf_data) => gltf_data,
                                                        Err(e) => {
                                                            send_error(&mut write, &format!("Failed to process Houdini JSON: {}", e)).await;
                                                            continue;
                                                        }
                                                    }
                                                } else {
                                                    raw_data
                                                };
                                                
                                                let additional_files_json = request.additional_files.as_ref().and_then(|files| {
                                                    serde_json::to_string(files).ok()
                                                });
                                                
                                                match insert_model(&final_data, request.name.as_deref(), &file_type, additional_files_json.as_deref()) {
                                                    Ok(_new_id) => {
                                                        match load_all_models_metadata() {
                                                            Ok(models) => {
                                                                let update = serde_json::to_string(&models).unwrap();
                                                                if let Err(e) = tx.send(update.clone()) {
                                                                    eprintln!("Broadcast error: {:?}", e);
                                                                }
                                                                if let Err(e) = write
                                                                    .send(Message::Text(update.into()))
                                                                    .await
                                                                {
                                                                    eprintln!("Send error: {:?}", e);
                                                                    break;
                                                                }
                                                            }
                                                            Err(e) => {
                                                                send_error(&mut write, &format!("Failed to load models after insert: {}", e)).await;
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        send_error(&mut write, &format!("Failed to insert model: {}", e)).await;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                send_error(&mut write, &format!("Invalid base64 data: {}", e)).await;
                                            }
                                        }
                                    }
                                }
                                "delete" => {
                                    if let Some(id) = request.id {
                                        match delete_model(id) {
                                            Ok(()) => {
                                                match load_all_models_metadata() {
                                                    Ok(models) => {
                                                        let update = serde_json::to_string(&models).unwrap();
                                                        if let Err(e) = tx.send(update.clone()) {
                                                            eprintln!("Broadcast error: {:?}", e);
                                                        }
                                                        if let Err(e) = write
                                                            .send(Message::Text(update.into()))
                                                            .await
                                                        {
                                                            eprintln!("Send error: {:?}", e);
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        send_error(&mut write, &format!("Failed to load models after delete: {}", e)).await;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                send_error(&mut write, &format!("Failed to delete model: {}", e)).await;
                                            }
                                        }
                                    }
                                }
                                _ => eprintln!("Unknown action: {}", request.action),
                            }
                        }
                        Err(e) => eprintln!("Failed to parse request: {}", e),
                    }
                } else if let Message::Ping(data) = message {
                    if let Err(e) = write.send(Message::Pong(data)).await {
                        eprintln!("Send pong error: {:?}", e);
                        break;
                    }
                } else if let Message::Close(_) = message {
                    break;
                }
            }
            Ok(update) = rx.recv() => {
                if let Err(e) = write.send(Message::Text(update.into())).await {
                    eprintln!("Forward error: {:?}", e);
                    break;
                }
            }
            else => {
                break;
            }
        }
    }
}

async fn send_error<S>(write: &mut S, message: &str)
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Debug,
{
    let error_response = serde_json::to_string(&serde_json::json!({ "error": message })).unwrap();
    if let Err(e) = write.send(Message::Text(error_response.into())).await {
        eprintln!("Error sending error: {:?}", e);
    }
}

fn detect_file_type(data: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(data) {
        let trimmed = text.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                if is_houdini_json(&json) {
                    return "houdini_json".to_string();
                }
                return "gltf".to_string();
            }
        }
    }
    
    if data.len() >= 4 && &data[0..4] == b"glTF" {
        return "gltf".to_string();
    }
    
    "gltf".to_string()
}

fn is_houdini_json(json: &serde_json::Value) -> bool {
    if let Some(array) = json.as_array() {
        for item in array {
            if let Some(key) = item.as_str() {
                match key {
                    "fileversion" | "pointcount" | "vertexcount" | "primitivecount" => return true,
                    _ => {}
                }
            }
        }
    }
    false
}

fn process_houdini_json(data: &[u8]) -> Result<Vec<u8>, String> {
    let json_str = std::str::from_utf8(data)
        .map_err(|e| format!("Invalid UTF-8 in JSON data: {}", e))?;
    
    let geometry = HoudiniJsonParser::parse_from_json(json_str)?;
    let gltf_json = HoudiniJsonParser::to_gltf_json(&geometry)?;
    
    Ok(gltf_json.into_bytes())
}

fn init_db() -> Result<Connection> {
    let conn = Connection::open("models.db")?;
    
    // Create table first if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            Name TEXT,
            model_data BLOB NOT NULL,
            file_type TEXT DEFAULT 'gltf',
            additional_files TEXT
        )",
        params![],
    )?;
    
    // Then try to add columns if they don't exist (for legacy databases)
    let _ = conn.execute(
        "ALTER TABLE models ADD COLUMN additional_files TEXT",
        params![],
    );
    
    let _ = conn.execute(
        "ALTER TABLE models ADD COLUMN file_type TEXT DEFAULT 'gltf'",
        params![],
    );
    
    let _ = conn.execute(
        "ALTER TABLE models ADD COLUMN Name TEXT",
        params![],
    );
    
    Ok(conn)
}

fn load_all_models_metadata() -> Result<Vec<ModelListItem>> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, COALESCE(file_type, 'gltf') FROM models")?;
    let model_iter = stmt.query_map(params![], |row| {
        Ok(ModelListItem {
            id: row.get(0)?,
            name: row.get(1)?,
            file_type: row.get(2)?,
        })
    })?;
    let mut models = Vec::new();
    for model in model_iter {
        models.push(model?);
    }
    Ok(models)
}

fn load_model_by_id(model_id: i32) -> Result<ModelData> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, model_data, COALESCE(file_type, 'gltf'), additional_files FROM models WHERE id = ?1")?;
    let model_data = stmt.query_row(params![model_id], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            name: row.get(1)?,
            model_data: row.get(2)?,
            file_type: row.get(3)?,
            additional_files: row.get(4)?,
        })
    })?;
    Ok(model_data)
}

fn insert_model(model_data: &[u8], name: Option<&str>, file_type: &str, additional_files: Option<&str>) -> Result<i32> {
    let conn = init_db()?;
    conn.execute(
        "INSERT INTO models (Name, model_data, file_type, additional_files) VALUES (?1, ?2, ?3, ?4)", 
        params![name, model_data, file_type, additional_files]
    )?;
    Ok(conn.last_insert_rowid() as i32)
}

fn delete_model(model_id: i32) -> Result<()> {
    let conn = init_db()?;
    let rows_affected = conn.execute("DELETE FROM models WHERE id = ?1", params![model_id])?;
    if rows_affected == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

fn check_model_exists_by_name(name: &str) -> Result<bool> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM models WHERE Name = ?1")?;
    let count: i32 = stmt.query_row(params![name], |row| row.get(0))?;
    Ok(count > 0)
}