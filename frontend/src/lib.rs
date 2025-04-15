use bevy::{
    pbr::{ CascadeShadowConfigBuilder, DirectionalLightShadowMap },
    prelude::*,
};
use bevy_panorbit_camera::{ PanOrbitCameraPlugin, PanOrbitCamera };
use bevy_egui::{ egui, EguiContexts, EguiPlugin };
use serde::{ Deserialize, Serialize };
use std::{
    path::Path,
    io::Write,
    fs::File,
    time::Duration
};
use tokio::sync::mpsc;
use tokio_tungstenite::{ connect_async_with_config, tungstenite::Message };
use futures_util::{ SinkExt, StreamExt };
use uuid::Uuid;
use base64::{ Engine as _, engine::general_purpose };
use rfd::FileDialog;

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    name: Option<String>,
    model_data: Option<String>, // base64-encoded
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModelResponse {
    id: i32,
    name: Option<String>,
    model_data: String, // base64-encoded
}

#[derive(Resource)]
struct ModelState {
    models: Vec<(i32, String, Option<String>)>, // (id, temp_file_path, name)
    model_entities: Vec<(i32, Entity)>,
}

#[derive(Resource)]
struct ModelUpdateReceiver(mpsc::Receiver<Vec<ModelResponse>>);

#[derive(Resource)]
struct UploadState {
    status: String,
    ws_tx: mpsc::Sender<String>,
    file_tx: mpsc::Sender<(String, Result<(Vec<u8>, Option<String>), String>)>,
    file_rx: mpsc::Receiver<(String, Result<(Vec<u8>, Option<String>), String>)>,
    model_name: String,
}

pub fn run() {
    App::new()
        .insert_resource(DirectionalLightShadowMap { size: 4096 })
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "PGS Renderman".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_plugins(PanOrbitCameraPlugin)
        .add_plugins(EguiPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (ui_system, handle_model_updates, handle_file_results))
        .add_systems(Startup, debug_resources)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Transform::from_translation(Vec3::new(-6.0, 5.0, 1.5)),
        PanOrbitCamera::default(),
    ));

    commands.spawn((
        DirectionalLight {
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            maximum_distance: 1.6,
            ..default()
        }
        .build(),
    ));

    commands.insert_resource(ModelState {
        models: vec![],
        model_entities: vec![],
    });

    let (update_tx, update_rx) = mpsc::channel(100);
    let (ws_tx, mut ws_rx) = mpsc::channel(100);
    let (file_tx, file_rx) = mpsc::channel(1);
    commands.insert_resource(ModelUpdateReceiver(update_rx));
    commands.insert_resource(UploadState {
        status: "Ready".to_string(),
        ws_tx,
        file_tx,
        file_rx,
        model_name: String::new(),
    });

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        rt.block_on(async {
            let connection_id = Uuid::new_v4().to_string();
            loop {
                let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
                config.max_message_size = Some(100 * 1024 * 1024); // 100 MB
                config.max_frame_size = Some(100 * 1024 * 1024);   // 100 MB
                config.accept_unmasked_frames = false;
                match connect_async_with_config("ws://127.0.0.1:8000/ws", Some(config), false).await {
                    Ok((mut ws_stream, _)) => {
                        let request = ModelRequest {
                            action: "get_all".to_string(),
                            id: None,
                            name: None,
                            model_data: None,
                        };
                        let request_str = serde_json::to_string(&request).unwrap();
                        if let Err(e) = ws_stream
                            .send(Message::Text(request_str.clone().into()))
                            .await
                        {
                            error!("Connection {}: Failed to send initial get_all request: {}", connection_id, e);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }

                        let mut ping_interval = tokio::time::interval(Duration::from_secs(10));

                        loop {
                            tokio::select! {
                                Some(message_result) = ws_stream.next() => {
                                    match message_result {
                                        Ok(Message::Text(text)) => {
                                            match serde_json::from_str::<Vec<ModelResponse>>(&text) {
                                                Ok(models) => {
                                                    if let Err(e) = update_tx.send(models).await {
                                                        error!("Connection {}: Failed to send models to channel: {}", connection_id, e);
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Connection {}: Failed to parse WebSocket message: {}", connection_id, e);
                                                }
                                            }
                                        }
                                        Ok(Message::Ping(_)) => {
                                            if let Err(e) = ws_stream.send(Message::Pong(vec![].into())).await {
                                                error!("Connection {}: Failed to send pong: {}", connection_id, e);
                                                break;
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(e) => {
                                            error!("Connection {}: WebSocket error: {}", connection_id, e);
                                            break;
                                        }
                                    }
                                }
                                _ = ping_interval.tick() => {
                                    if let Err(e) = ws_stream.send(Message::Ping(vec![].into())).await {
                                        error!("Connection {}: Failed to send ping: {}", connection_id, e);
                                        break;
                                    }
                                }
                                Some(upload_request) = ws_rx.recv() => {
                                    if let Err(e) = ws_stream.send(Message::Text(upload_request.into())).await {
                                        error!("Connection {}: Failed to send upload request: {}", connection_id, e);
                                        break;
                                    }
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    Err(e) => {
                        error!("Connection {}: WebSocket connection failed: {}", connection_id, e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
    });
}

fn ui_system(
    mut contexts: EguiContexts,
    state: Res<ModelState>,
    mut upload_state: ResMut<UploadState>,
) {
    // Model List Window
    egui::Window::new("Model List").show(contexts.ctx_mut(), |ui| {
        ui.label("Loaded Models:");
        for (id, _path, name) in &state.models {
            let display_name = name
                .as_ref()
                .map_or_else(|| format!("Model {}", id), |n| n.clone());
            ui.label(format!("{}. {}", id, display_name));
        }
    });

    // Upload Model Window
    egui::Window::new("Upload Model").show(contexts.ctx_mut(), |ui| {
        ui.label("Model Name:");
        ui.text_edit_singleline(&mut upload_state.model_name);
        ui.label("Select a .gltf file to upload:");
        if ui.button("Choose File").clicked() {
            if upload_state.status != "Uploading..." {
                upload_state.status = "Uploading...".to_string();
                let file_tx = upload_state.file_tx.clone();
                std::thread::spawn(move || {
                    let (path_str, result) = if let Some(path) = FileDialog::new()
                        .add_filter("GLTF Files", &["gltf"])
                        .pick_file()
                    {
                        let path_str = path.to_string_lossy().to_string();
                        let file_name = Path::new(&path_str)
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .map(|s| s.to_string());
                        match std::fs::read(&path) {
                            Ok(data) => (path_str, Ok((data, file_name))),
                            Err(e) => (path_str, Err(format!("Failed to read file: {}", e))),
                        }
                    } else {
                        ("".to_string(), Err("No file selected".to_string()))
                    };
                    if let Err(e) = file_tx.blocking_send((path_str, result)) {
                        error!("Failed to send file result: {}", e);
                    }
                });
            }
        }
        ui.label(&upload_state.status);
    });
}

fn handle_file_results(
    mut upload_state: ResMut<UploadState>,
) {
    while let Ok((path, result)) = upload_state.file_rx.try_recv() {
        match result {
            Ok((data, file_name)) => {
                // Set model_name to file_name if not user-edited
                if upload_state.model_name.is_empty() {
                    if let Some(name) = &file_name {
                        upload_state.model_name = name.clone();
                    }
                }
                let base64_data = general_purpose::STANDARD.encode(&data);
                let request = ModelRequest {
                    action: "insert".to_string(),
                    id: None,
                    name: if upload_state.model_name.is_empty() {
                        file_name
                    } else {
                        Some(upload_state.model_name.clone())
                    },
                    model_data: Some(base64_data),
                };
                let request_str = serde_json::to_string(&request).unwrap();
                if let Err(e) = upload_state.ws_tx.try_send(request_str) {
                    upload_state.status = format!("Failed to queue upload: {}", e);
                    error!("Failed to queue upload: {}", e);
                } else {
                    upload_state.status = "Upload queued".to_string();
                    upload_state.model_name.clear(); // Clear name for next upload
                }
            }
            Err(e) => {
                upload_state.status = e.clone();
                if e != "No file selected" {
                    error!("File error for {}: {}", path, e);
                }
            }
        }
    }
}

fn handle_model_updates(
    mut state: ResMut<ModelState>,
    mut commands: Commands,
    mut receiver: ResMut<ModelUpdateReceiver>,
    asset_server: Res<AssetServer>,
    mut upload_state: ResMut<UploadState>,
) {
    while let Ok(models) = receiver.0.try_recv() {
        info!("Updating scene with {} models", models.len());

        // Update upload status if new models detected
        if !models.is_empty() && upload_state.status == "Upload queued" {
            upload_state.status = "Upload successful".to_string();
        }

        // Remove entities for models no longer in the list
        state.model_entities.retain(|(id, entity)| {
            if models.iter().any(|m| m.id == *id) {
                true
            } else {
                info!("Removing model ID={}", id);
                commands.entity(*entity).despawn();
                false
            }
        });

        // Create new temp files and load models
        let mut new_models = vec![];
        for model in models {
            if !state.model_entities.iter().any(|(id, _)| *id == model.id) {
                info!("Loading new model: ID={}", model.id);
                // Decode base64
                match general_purpose::STANDARD.decode(&model.model_data) {
                    Ok(model_data) => {
                        // Create temp file
                        let temp_dir = std::env::temp_dir();
                        let temp_file_name = format!("model_{}.gltf", model.id);
                        let temp_path = temp_dir.join(&temp_file_name);
                        let temp_path_str = temp_path.to_str().expect("Invalid temp path").to_string();

                        // Write to temp file
                        let mut file = File::create(&temp_path).expect("Failed to create temp file");
                        file.write_all(&model_data).expect("Failed to write temp file");

                        // Load into Bevy
                        let entity = commands
                            .spawn(SceneRoot(asset_server.load(
                                GltfAssetLabel::Scene(0).from_asset(temp_path_str.clone()),
                            )))
                            .id();
                        state.model_entities.push((model.id, entity));
                        new_models.push((model.id, temp_path_str, model.name));
                    }
                    Err(e) => {
                        error!("Failed to decode base64 for model ID={}: {}", model.id, e);
                    }
                }
            } else {
                // Keep existing temp file path and update name
                if let Some((_, temp_path, _)) = state.models.iter().find(|(id, _, _)| *id == model.id) {
                    new_models.push((model.id, temp_path.clone(), model.name));
                }
            }
        }

        // Update models list with temp file paths and names
        state.models = new_models;
    }
}

fn debug_resources(world: &World) {
    if world.get_resource::<Assets<Shader>>().is_some() {
        info!("Assets<Shader> resource is available");
    } else {
        error!("Assets<Shader> resource is NOT available");
    }
}