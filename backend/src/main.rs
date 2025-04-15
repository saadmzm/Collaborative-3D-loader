use futures_util::{ SinkExt, StreamExt };
use rusqlite::{ params, Connection, Result };
use serde::{ Deserialize, Serialize };
use std::{ collections::HashSet, time::Duration };
use tokio::{
    net::{ TcpListener, TcpStream },
    sync::broadcast::{ self, Sender }
};
use tokio_tungstenite::{ accept_async, tungstenite::Message };

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
struct ModelResponse {
    id: i32,
    path: String,
}

#[derive(Debug)]
struct ModelData {
    id: i32,
    path: String,
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
                        .map(|m| ModelResponse { id: m.id, path: m.path })
                        .collect();
                    if current_models != last_models {
                        println!("Detected database change, broadcasting updated models");
                        let updated_list: Vec<ModelResponse> = current_models.iter().cloned().collect();
                        let update = serde_json::to_string(&updated_list).unwrap();
                        if let Err(e) = tx_clone.send(update) {
                            eprintln!("Broadcast error: {:?}", e);
                        }
                        last_models = current_models;
                    }
                }
                Err(e) => eprintln!("Failed to poll models: {}", e),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    while let Ok((stream, addr)) = listener.accept().await {
        let tx = tx.clone();
        println!("New connection from: {}", addr);
        tokio::spawn(handle_connection(stream, tx));
    }
}

async fn handle_connection(stream: TcpStream, tx: Sender<String>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Failed to accept WebSocket connection: {:?}", e);
            return;
        }
    };
    println!("WebSocket connection established");

    let (mut write, mut read) = ws_stream.split();
    let mut rx = tx.subscribe();

    loop {
        tokio::select! {
            Some(Ok(message)) = read.next() => {
                if let Message::Text(text) = message {
                    println!("Received request: {}", text);
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
                                                    .send(Message::Text(serde_json::to_string(&response).unwrap().into()))
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
                                                    path: m.path,
                                                })
                                                .collect();
                                            if let Err(e) = write
                                                .send(Message::Text(serde_json::to_string(&response).unwrap().into()))
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
                                    if let Some(path) = request.path {
                                        match insert_model(&path) {
                                            Ok(new_id) => {
                                                let new_model = ModelResponse {
                                                    id: new_id,
                                                    path,
                                                };
                                                let update = serde_json::to_string(&new_model).unwrap();
                                                println!("Broadcasting new model: {}", update);
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
                                }
                                _ => eprintln!("Unknown action: {}", request.action),
                            }
                        }
                        Err(e) => eprintln!("Failed to parse request: {}", e),
                    }
                } else if let Message::Ping(data) = message {
                    println!("Received ping, sending pong");
                    if let Err(e) = write.send(Message::Pong(data)).await {
                        eprintln!("Send pong error: {:?}", e);
                        break;
                    }
                } else if let Message::Close(_)= message {
                    println!("Received close message, closing connection");
                    break;
                }
            }
            Ok(update) = rx.recv() => {
                println!("Forwarding update to client: {}", update);
                if let Err(e) = write.send(Message::Text(update.into())).await {
                    eprintln!("Forward error: {:?}", e);
                    break;
                }
            }
            else => {
                println!("No more messages or error in read stream, closing connection");
                break;
            }
        }
    }
    println!("WebSocket connection closed");
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

fn insert_model(path: &str) -> Result<i32> {
    let conn = init_db()?;
    conn.execute("INSERT INTO models (path) VALUES (?1)", params![path])?;
    Ok(conn.last_insert_rowid() as i32)
}