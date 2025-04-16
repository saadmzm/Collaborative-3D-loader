use bevy::{
    pbr::{ CascadeShadowConfigBuilder, DirectionalLightShadowMap },
    prelude::*,
};
use bevy_panorbit_camera::{ PanOrbitCameraPlugin, PanOrbitCamera };
use bevy_egui::{ egui, EguiContexts, EguiPlugin };
use serde::{ Deserialize, Serialize };
use std::{
    time::Duration,
    fs::File,
    io::Write,
    path::Path,
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
    selected_model: Option<i32>, // None for "All Models", Some(id) for single model
}

#[derive(Resource, Default)]
struct LastSelectedModel {
    id: Option<i32>,
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
        .add_systems(Update, (
            ui_system,
            handle_model_updates,
            handle_file_results,
            update_scene_on_selection,
            block_camera_on_egui
        ))
        .add_systems(Startup, debug_resources)
        .run();
}

fn block_camera_on_egui(
    mut camera_query: Query<&mut PanOrbitCamera>,
    mut egui_context: EguiContexts,
) {
    let is_egui_active = egui_context.ctx_mut().wants_pointer_input();
    for mut camera in camera_query.iter_mut() {
        camera.enabled = !is_egui_active;
    }
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
        selected_model: None, // Explicitly None for All Models
    });
    commands.insert_resource(LastSelectedModel::default());

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        rt.block_on(async {
            let connection_id = Uuid::new_v4().to_string();
            loop {
                let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
                config.max_message_size = Some(100 * 1024 * 1024);
                config.max_frame_size = Some(100 * 1024 * 1024);
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
    // Model List Window (default position, left side)
    egui::Window::new("Model List").show(contexts.ctx_mut(), |ui| {
        ui.label("Loaded Models:");
        for (id, _path, name) in &state.models {
            let display_name = name
                .as_ref()
                .map_or_else(|| format!("Model {}", id), |n| n.clone());
            ui.horizontal(|ui| {
                ui.label(format!("{}. {}", id, display_name));
                if ui.button("Delete").clicked() {
                    let request = ModelRequest {
                        action: "delete".to_string(),
                        id: Some(*id),
                        name: None,
                        model_data: None,
                    };
                    let request_str = serde_json::to_string(&request).unwrap();
                    if let Err(e) = upload_state.ws_tx.try_send(request_str) {
                        error!("Failed to send delete request for ID {}: {}", id, e);
                    }
                }
            });
        }
    });

    // Upload Model Window (positioned on the right)
    egui::Window::new("Upload Model")
        .default_pos([1000.0, 50.0]) // Right side for 1280x720 window
        .show(contexts.ctx_mut(), |ui| {
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

    // Model Selection Window (centered)
    egui::Window::new("Model Selection")
        .default_pos([640.0, 360.0]) // Center for 1280x720 window
        .show(contexts.ctx_mut(), |ui| {
            let selected_text = match upload_state.selected_model {
                None => "All Models".to_string(),
                Some(id) => state
                    .models
                    .iter()
                    .find(|(model_id, _, _)| *model_id == id)
                    .map(|(_, _, name)| {
                        name.as_ref()
                            .map_or_else(|| format!("Model {}", id), |n| format!("{}: {}", id, n))
                    })
                    .unwrap_or_else(|| "Model Not Found".to_string()),
            };

            egui::ComboBox::from_label("Select Model")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    // Option for All Models
                    ui.selectable_value(&mut upload_state.selected_model, None, "All Models");
                    // Options for individual models
                    for (id, _, name) in &state.models {
                        let display_name = name
                            .as_ref()
                            .map_or_else(|| format!("Model {}", id), |n| format!("{}: {}", id, n));
                        ui.selectable_value(&mut upload_state.selected_model, Some(*id), display_name);
                    }
                });
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

fn update_scene_on_selection(
    mut commands: Commands,
    mut state: ResMut<ModelState>,
    upload_state: Res<UploadState>,
    mut last_selected: ResMut<LastSelectedModel>,
    asset_server: Res<AssetServer>,
) {
    // Always check if scene needs update
    let should_update = last_selected.id != upload_state.selected_model ||
        state.model_entities.iter().map(|(id, _)| *id).collect::<Vec<_>>() !=
        match upload_state.selected_model {
            Some(id) => state.models.iter().filter(|(mid, _, _)| *mid == id).map(|(id, _, _)| *id).collect::<Vec<_>>(),
            None => state.models.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        };

    if should_update {
        info!("Updating scene, selected: {:?}", upload_state.selected_model);

        // Despawn all existing entities
        for (_, entity) in state.model_entities.drain(..) {
            info!("Despawning entity for model");
            commands.entity(entity).despawn();
        }
        state.model_entities.clear();

        // Load models based on selection
        let filtered_models = match upload_state.selected_model {
            Some(selected_id) => state
                .models
                .iter()
                .filter(|(id, _, _)| *id == selected_id)
                .cloned()
                .collect::<Vec<_>>(),
            None => state.models.clone(),
        };

        // Spawn filtered models
        for (id, temp_path_str, _name) in filtered_models {
            info!("Loading model ID={} at path {}", id, temp_path_str);
            let entity = commands
                .spawn(SceneRoot(asset_server.load(
                    GltfAssetLabel::Scene(0).from_asset(temp_path_str.clone()),
                )))
                .id();
            state.model_entities.push((id, entity));
        }

        // Update last selected
        last_selected.id = upload_state.selected_model;
    }
}

fn handle_model_updates(
    mut state: ResMut<ModelState>,
    mut receiver: ResMut<ModelUpdateReceiver>,
    mut upload_state: ResMut<UploadState>,
    mut last_selected: ResMut<LastSelectedModel>,
) {
    while let Ok(models) = receiver.0.try_recv() {
        info!("Received {} models, selected: {:?}", models.len(), upload_state.selected_model);

        // Update upload status if new models detected
        if !models.is_empty() && upload_state.status == "Upload queued" {
            upload_state.status = "Upload successful".to_string();
        }

        // Update state.models with all models to keep dropdown accurate
        let mut new_models = vec![];
        for model in models {
            let temp_path = state
                .models
                .iter()
                .find(|(id, _, _)| *id == model.id)
                .map(|(_, path, _)| path.clone())
                .unwrap_or_else(|| {
                    let temp_dir = std::env::temp_dir();
                    let temp_file_name = format!("model_{}.gltf", model.id);
                    let temp_path = temp_dir.join(&temp_file_name);
                    let temp_path_str = temp_path.to_str().expect("Invalid temp path").to_string();

                    // Write to temp file
                    match general_purpose::STANDARD.decode(&model.model_data) {
                        Ok(model_data) => {
                            let mut file = File::create(&temp_path).expect("Failed to create temp file");
                            file.write_all(&model_data).expect("Failed to write temp file");
                        }
                        Err(e) => {
                            error!("Failed to decode base64 for model ID={}: {}", model.id, e);
                        }
                    }
                    temp_path_str
                });
            new_models.push((model.id, temp_path, model.name));
        }
        state.models = new_models;

        // Trigger scene update
        last_selected.id = None;

        // Reset selection if model not found
        if let Some(selected_id) = upload_state.selected_model {
            if !state.models.iter().any(|(id, _, _)| *id == selected_id) {
                info!("Selected model ID={} not found, resetting to All Models", selected_id);
                upload_state.selected_model = None;
            }
        }
    }
}

fn debug_resources(world: &World) {
    if world.get_resource::<Assets<Shader>>().is_some() {
        info!("Assets<Shader> resource is available");
    } else {
        error!("Assets<Shader> resource is NOT available");
    }
}