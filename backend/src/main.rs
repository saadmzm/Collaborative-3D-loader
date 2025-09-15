use futures_util::{ SinkExt, StreamExt };
use rusqlite::{ params, Connection, Result };
use serde::{ Deserialize, Serialize };
use std::{
    collections::HashSet,
    time::Duration
};
use tokio::{
    net::{ TcpListener, TcpStream },
    sync::broadcast::{ self, Sender }
};
use tokio_tungstenite::{ accept_async_with_config, tungstenite::Message };
use base64::{ Engine as _, engine::general_purpose };

// Add this module for Houdini JSON support
mod houdini_json;
use houdini_json::HoudiniJsonParser;

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    name: Option<String>,
    model_data: Option<String>, // base64-encoded model data for insert
    file_type: Option<String>,  // New field to specify file type: "gltf" or "houdini_json"
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
struct ModelResponse {
    id: i32,
    name: Option<String>,
    model_data: String, // base64-encoded model data
    file_type: String,  // "gltf" or "houdini_json" 
}

#[derive(Debug)]
struct ModelData {
    id: i32,
    name: Option<String>,
    model_data: Vec<u8>, // raw binary data
    file_type: String,   // "gltf" or "houdini_json"
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:8000").await.expect("Failed to bind");
    println!("Backend WebSocket server running on ws://127.0.0.1:8000/ws");
    println!("Supported formats: GLTF (.gltf) and Houdini JSON (.json)");

    let (tx, _) = broadcast::channel(16);

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut last_models: HashSet<ModelResponse> = HashSet::new();
        loop {
            match load_all_models() {
                Ok(models) => {
                    let current_models: HashSet<ModelResponse> = models
                        .into_iter()
                        .map(|m| ModelResponse {
                            id: m.id,
                            name: m.name,
                            model_data: general_purpose::STANDARD.encode(&m.model_data),
                            file_type: m.file_type,
                        })
                        .collect();
                    if current_models != last_models {
                        let updated_list: Vec<ModelResponse> = current_models.iter().cloned().collect();
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
    config.max_message_size = Some(100 * 1024 * 1024); // 100 MB
    config.max_frame_size = Some(100 * 1024 * 1024);   // 100 MB
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
                                                let response = ModelResponse {
                                                    id: model.id,
                                                    name: model.name,
                                                    model_data: general_purpose::STANDARD.encode(&model.model_data),
                                                    file_type: model.file_type,
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
                                    match load_all_models() {
                                        Ok(models) => {
                                            let response: Vec<ModelResponse> = models
                                                .into_iter()
                                                .map(|m| ModelResponse {
                                                    id: m.id,
                                                    name: m.name,
                                                    model_data: general_purpose::STANDARD.encode(&m.model_data),
                                                    file_type: m.file_type,
                                                })
                                                .collect();
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
                                            send_error(&mut write, &format!("Failed to load models: {}", e)).await;
                                        }
                                    }
                                }
                                "insert" => {
                                    if let Some(base64_data) = request.model_data {
                                        match general_purpose::STANDARD.decode(&base64_data) {
                                            Ok(raw_data) => {
                                                let file_type = request.file_type.unwrap_or_else(|| {
                                                    // Auto-detect file type
                                                    detect_file_type(&raw_data)
                                                });
                                                
                                                let final_data = if file_type == "houdini_json" {
                                                    // Convert Houdini JSON to GLTF for compatibility
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
                                                
                                                match insert_model(&final_data, request.name.as_deref(), &file_type) {
                                                    Ok(_new_id) => {
                                                        // Broadcast updated model list instead of single model
                                                        match load_all_models() {
                                                            Ok(models) => {
                                                                let response: Vec<ModelResponse> = models
                                                                    .into_iter()
                                                                    .map(|m| ModelResponse {
                                                                        id: m.id,
                                                                        name: m.name,
                                                                        model_data: general_purpose::STANDARD.encode(&m.model_data),
                                                                        file_type: m.file_type,
                                                                    })
                                                                    .collect();
                                                                let update = serde_json::to_string(&response).unwrap();
                                                                if let Err(e) = tx.send(update) {
                                                                    eprintln!("Broadcast error: {:?}", e);
                                                                }
                                                                if let Err(e) = write
                                                                    .send(Message::Text(serde_json::to_string(&response).unwrap().into()))
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
                                                // Broadcast updated model list
                                                match load_all_models() {
                                                    Ok(models) => {
                                                        let response: Vec<ModelResponse> = models
                                                            .into_iter()
                                                            .map(|m| ModelResponse {
                                                                id: m.id,
                                                                name: m.name,
                                                                model_data: general_purpose::STANDARD.encode(&m.model_data),
                                                                file_type: m.file_type,
                                                            })
                                                            .collect();
                                                        let update = serde_json::to_string(&response).unwrap();
                                                        println!("Broadcasting model list after insert: {} models", response.len());
                                                        if let Err(e) = tx.send(update) {
                                                            eprintln!("Broadcast error: {:?}", e);
                                                        }
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
    // Check if it's JSON by looking for opening brace or bracket
    if let Ok(text) = std::str::from_utf8(data) {
        let trimmed = text.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            // Try to parse as JSON and look for Houdini-specific fields
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                if is_houdini_json(&json) {
                    return "houdini_json".to_string();
                }
                // Could be regular JSON, but default to GLTF for now
                return "gltf".to_string();
            }
        }
    }
    
    // Check for GLTF binary signature
    if data.len() >= 4 && &data[0..4] == b"glTF" {
        return "gltf".to_string();
    }
    
    // Default to GLTF
    "gltf".to_string()
}

fn is_houdini_json(json: &serde_json::Value) -> bool {
    // Check if it's a Houdini JSON by looking for characteristic fields
    if let Some(array) = json.as_array() {
        // Look for Houdini-specific keys in the array
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
    
    // Migration: Add file_type column if it doesn't exist
    conn.execute(
        "ALTER TABLE models ADD COLUMN file_type TEXT DEFAULT 'gltf'",
        params![],
    )
    .unwrap_or_else(|e| {
        if !e.to_string().contains("duplicate column name") {
            panic!("Failed to add file_type column: {}", e);
        }
        0
    });
    
    // Migration: Add Name column if it doesn't exist (existing migration)
    conn.execute(
        "ALTER TABLE models ADD COLUMN Name TEXT",
        params![],
    )
    .unwrap_or_else(|e| {
        if !e.to_string().contains("duplicate column name") {
            panic!("Failed to add Name column: {}", e);
        }
        0
    });
    
    // Create table with new schema
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            Name TEXT,
            model_data BLOB NOT NULL,
            file_type TEXT DEFAULT 'gltf'
        )",
        params![],
    )?;
    
    Ok(conn)
}

fn load_model_by_id(model_id: i32) -> Result<ModelData> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, model_data, COALESCE(file_type, 'gltf') FROM models WHERE id = ?1")?;
    let model_data = stmt.query_row(params![model_id], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            name: row.get(1)?,
            model_data: row.get(2)?,
            file_type: row.get(3)?,
        })
    })?;
    Ok(model_data)
}

fn load_all_models() -> Result<Vec<ModelData>> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, model_data, COALESCE(file_type, 'gltf') FROM models")?;
    let model_iter = stmt.query_map(params![], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            name: row.get(1)?,
            model_data: row.get(2)?,
            file_type: row.get(3)?,
        })
    })?;
    let mut models = Vec::new();
    for model in model_iter {
        models.push(model?);
    }
    Ok(models)
}

fn insert_model(model_data: &[u8], name: Option<&str>, file_type: &str) -> Result<i32> {
    let conn = init_db()?;
    conn.execute(
        "INSERT INTO models (Name, model_data, file_type) VALUES (?1, ?2, ?3)", 
        params![name, model_data, file_type]
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