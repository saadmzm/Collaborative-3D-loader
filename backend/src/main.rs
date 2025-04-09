use futures_util::{SinkExt, StreamExt};
use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};

// Message formats for WebSocket communication
#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String, // "get_by_id" or "get_all"
    id: Option<i32>,
}

#[derive(Serialize, Deserialize)]
struct ModelResponse {
    id: i32,
    path: String,
}

// Model data from the database
#[derive(Debug)]
struct ModelData {
    id: i32,
    path: String,
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("17.76.57.76:8000").await.expect("Failed to bind");
    println!("Backend WebSocket server running on ws://17.76.57.76:8000/ws");

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(handle_connection(stream));
    }
}

async fn handle_connection(stream: TcpStream) {
    let ws_stream = accept_async(stream)
        .await
        .expect("Failed to accept WebSocket connection");
    let (mut write, mut read) = ws_stream.split();

    while let Some(Ok(message)) = read.next().await {
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
                                            path: model.path,
                                        };
                                        if let Err(e) = write
                                            .send(Message::Text(
                                                serde_json::to_string(&response)
                                                    .expect("Failed to serialize response"),
                                            ))
                                            .await
                                        {
                                            eprintln!("Failed to send response: {}", e);
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to load model: {}", e);
                                        let error_response = serde_json::to_string(&serde_json::json!({
                                            "error": format!("Model not found: {}", e)
                                        }))
                                        .expect("Failed to serialize error");
                                        if let Err(e) = write.send(Message::Text(error_response)).await {
                                            eprintln!("Failed to send error response: {}", e);
                                            break;
                                        }
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
                                            path: m.path,
                                        })
                                        .collect();
                                    if let Err(e) = write
                                        .send(Message::Text(
                                            serde_json::to_string(&response)
                                                .expect("Failed to serialize response"),
                                        ))
                                        .await
                                    {
                                        eprintln!("Failed to send response: {}", e);
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Failed to load models: {}", e);
                                    let error_response = serde_json::to_string(&serde_json::json!({
                                        "error": format!("Failed to load models: {}", e)
                                    }))
                                    .expect("Failed to serialize error");
                                    if let Err(e) = write.send(Message::Text(error_response)).await {
                                        eprintln!("Failed to send error response: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        _ => {
                            eprintln!("Unknown action: {}", request.action);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to parse request: {}", e);
                }
            }
        }
    }
}

fn init_db() -> Result<Connection> {
    let conn = Connection::open("models.db")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL
        )",
        params![],
    )?;
    Ok(conn)
}

fn load_model_by_id(model_id: i32) -> Result<ModelData> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, path FROM models WHERE id = ?1")?;
    let model_data = stmt.query_row(params![model_id], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            path: row.get(1)?,
        })
    })?;
    Ok(model_data)
}

fn load_all_models() -> Result<Vec<ModelData>> {
    let conn = init_db()?;
    let mut stmt = conn.prepare("SELECT id, path FROM models")?;
    let model_iter = stmt.query_map(params![], |row| {
        Ok(ModelData {
            id: row.get(0)?,
            path: row.get(1)?,
        })
    })?;
    let mut models = Vec::new();
    for model in model_iter {
        models.push(model?);
    }
    Ok(models)
}