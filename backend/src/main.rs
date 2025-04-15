use futures_util::{ SinkExt, StreamExt };
use rusqlite::{ params, Connection, Result };
use serde::{ Deserialize, Serialize };
use std::{ collections::HashSet, time::Duration };
use tokio::{
    net::{ TcpListener, TcpStream },
    sync::broadcast::{ self, Sender }
};
use tokio_tungstenite::{ accept_async_with_config, tungstenite::Message };
use base64::{ Engine as _, engine::general_purpose };

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    name: Option<String>,       // New field for model name
    model_data: Option<String>, // base64-encoded model data for insert
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
struct ModelResponse {
    id: i32,
    name: Option<String>,      // New field for model name
    model_data: String,        // base64-encoded model data
}

#[derive(Debug)]
struct ModelData {
    id: i32,
    name: Option<String>, // New field for model name
    model_data: Vec<u8>,  // raw binary data
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:8000").await.expect("Failed to bind");
    println!("Backend WebSocket server running on ws://127.0.0.1:8000/ws");

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
                        })
                        .collect();
                    if current_models != last_models {
                        let updated_list: Vec<ModelResponse> = current_models.iter().cloned().collect();
                        let update = serde_json::to_string(&updated_list).unwrap();
                        if let Err(e) = tx_clone.send(update) {
                            eprintln!("Broadcast error: {}", e);
                        }//smzm
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
                                "get_by_id" => {//saad
                                    if let Some(id) = request.id {
                                        match load_model_by_id(id) {
                                            Ok(model) => {
                                                let response = ModelResponse {
                                                    id: model.id,
                                                    name: model.name,
                                                    model_data: general_purpose::STANDARD.encode(&model.model_data),                                                                                                                                    //Made by Saad Moazzam
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
                                }//mzm
                                "get_all" => {
                                    match load_all_models() {
                                        Ok(models) => {
                                            let response: Vec<ModelResponse> = models
                                                .into_iter()
                                                .map(|m| ModelResponse {
                                                    id: m.id,
                                                    name: m.name,
                                                    model_data: general_purpose::STANDARD.encode(&m.model_data),
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
                                            Ok(model_data) => {
                                                match insert_model(&model_data, request.name.as_deref()) {
                                                    Ok(new_id) => {
                                                        let new_model = ModelResponse {
                                                            id: new_id,
                                                            name: request.name,
                                                            model_data: base64_data,
                                                        };
                                                        let update = serde_json::to_string(&new_model).unwrap();
                                                        if let Err(e) = tx.send(update) {
                                                            eprintln!("Broadcast error: {:?}", e);
                                                        }
                                                        if let Err(e) = write
                                                            .send(Message::Text(serde_json::to_string(&new_model).unwrap().into()))
                                                            .await
                                                        {
                                                            eprintln!("Send error: {:?}", e);
                                                            break;
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

fn init_db() -> Result<Connection> {
    let conn = Connection::open("models.db")?;
    // Migration: Add Name column if it doesn't exist
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
            model_data BLOB NOT NULL
        )",
        params![],
    )?;
    Ok(conn)
}

fn load_model_by_id(model_id: i32) -> Result<ModelData> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, model_data FROM models WHERE id = ?1")?;
    let model_data = stmt.query_row(params![model_id], |row| {
        Ok(ModelData {//made by saad moazzam
            id: row.get(0)?,
            name: row.get(1)?,
            model_data: row.get(2)?,
        })
    })?;
    Ok(model_data)
}

fn load_all_models() -> Result<Vec<ModelData>> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, Name, model_data FROM models")?;
    let model_iter = stmt.query_map(params![], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            name: row.get(1)?,
            model_data: row.get(2)?,
        })
    })?;
    let mut models = Vec::new();
    for model in model_iter {
        models.push(model?);
    }//mzm
    Ok(models)
}

fn insert_model(model_data: &[u8], name: Option<&str>) -> Result<i32> {
    let conn = init_db()?;
    conn.execute("INSERT INTO models (Name, model_data) VALUES (?1, ?2)", params![name, model_data])?;
    Ok(conn.last_insert_rowid() as i32)
}