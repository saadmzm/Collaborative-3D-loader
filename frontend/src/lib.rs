use bevy::{
    pbr::{ CascadeShadowConfigBuilder, DirectionalLightShadowMap },
    prelude::*,
    render::{camera::Viewport, view::RenderLayers},
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

#[derive(Component)]
enum ViewerCamera {
    Viewer1,
    Viewer2,
}

#[derive(Component, Clone, Copy, PartialEq)]
enum ViewerLayer {
    Viewer1,
    Viewer2,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct AdditionalFile {
    filename: String,
    data: String, // base64-encoded
}

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
    name: Option<String>,
    model_data: Option<String>, // base64-encoded
    file_type: Option<String>,
    additional_files: Option<Vec<AdditionalFile>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModelResponse {
    id: i32,
    name: Option<String>,
    model_data: String, // base64-encoded
    file_type: String,  // "gltf" or "houdini_json"
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_files: Option<Vec<AdditionalFile>>,
}

#[derive(Resource)]
struct ModelState {
    models: Vec<(i32, String, Option<String>, String)>, // (id, temp_file_path, name, file_type)
    viewer1_entities: Vec<(i32, Entity)>,
    viewer2_entities: Vec<(i32, Entity)>,
}

#[derive(Resource)]
struct ModelUpdateReceiver(mpsc::Receiver<Vec<ModelResponse>>);

#[derive(Resource)]
struct UploadState {
    status: String,
    ws_tx: mpsc::Sender<String>,
    file_tx: mpsc::Sender<(String, Result<(Vec<u8>, Option<String>, String, Vec<AdditionalFile>), String>)>,
    file_rx: mpsc::Receiver<(String, Result<(Vec<u8>, Option<String>, String, Vec<AdditionalFile>), String>)>,
    model_name: String,
    viewer1_selected_model: Option<i32>,
    viewer2_selected_model: Option<i32>,
    file_filter: String,
}

#[derive(Resource, Default)]
struct LastSelectedModel {
    viewer1_id: Option<i32>,
    viewer2_id: Option<i32>,
    filter: String,
}

pub fn run() {
    App::new()
        .insert_resource(DirectionalLightShadowMap { size: 4096 })
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "PGS Renderman - GLTF & Houdini JSON Viewer (Dual View)".to_string(),
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
            block_camera_on_egui,
            setup_viewports,
            assign_render_layers,
        ))
        .add_systems(Startup, debug_resources)
        .run();
}

fn assign_render_layers(
    parent_query: Query<(Entity, &ViewerLayer, &RenderLayers)>,
    children_query: Query<&Children>,
    entity_query: Query<Entity, Without<RenderLayers>>,
    mut commands: Commands,
) {
    for (parent_entity, _viewer_layer, parent_render_layer) in parent_query.iter() {
        propagate_render_layers_to_children(&mut commands, parent_entity, parent_render_layer.clone(), &children_query, &entity_query);
    }
}

fn propagate_render_layers_to_children(
    commands: &mut Commands,
    entity: Entity,
    render_layer: RenderLayers,
    children_query: &Query<&Children>,
    entity_query: &Query<Entity, Without<RenderLayers>>,
) {
    if let Ok(children) = children_query.get(entity) {
        for child in children.iter() {
            if entity_query.get(*child).is_ok() {
                commands.entity(*child).insert(render_layer.clone());
            }
            propagate_render_layers_to_children(commands, *child, render_layer.clone(), children_query, entity_query);
        }
    }
}

fn setup_viewports(
    windows: Query<&Window>,
    mut cameras: Query<(&mut Camera, &ViewerCamera)>,
) {
    let window = windows.single();
    let window_width = window.physical_width();
    let window_height = window.physical_height();
    
    let half_width = window_width / 2;

    for (mut camera, viewer_camera) in cameras.iter_mut() {
        match viewer_camera {
            ViewerCamera::Viewer1 => {
                camera.viewport = Some(Viewport {
                    physical_position: UVec2::new(0, 0),
                    physical_size: UVec2::new(half_width, window_height),
                    ..default()
                });
            }
            ViewerCamera::Viewer2 => {
                camera.viewport = Some(Viewport {
                    physical_position: UVec2::new(half_width, 0),
                    physical_size: UVec2::new(half_width, window_height),
                    ..default()
                });
            }
        }
    }
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
        Camera3d::default(),
        Camera {
            order: 0,
            ..default()
        },
        Transform::from_translation(Vec3::new(-6.0, 5.0, 1.5)),
        PanOrbitCamera::default(),
        ViewerCamera::Viewer1,
        RenderLayers::layer(0),
    ));

    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            ..default()
        },
        Transform::from_translation(Vec3::new(-6.0, 5.0, 1.5)),
        PanOrbitCamera::default(),
        ViewerCamera::Viewer2,
        RenderLayers::layer(1),
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
        RenderLayers::layer(0).with(1),
    ));

    commands.insert_resource(ModelState {
        models: vec![],
        viewer1_entities: vec![],
        viewer2_entities: vec![],
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
        viewer1_selected_model: None,
        viewer2_selected_model: None,
        file_filter: "All".to_string(),
    });
    commands.insert_resource(LastSelectedModel {
        viewer1_id: None,
        viewer2_id: None,
        filter: "All".to_string(),
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
                            file_type: None,
                            additional_files: None,
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
    egui::Window::new("Model List").show(contexts.ctx_mut(), |ui| {
        ui.label("File Type Filter:");
        egui::ComboBox::from_label("Filter")
            .selected_text(&upload_state.file_filter)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut upload_state.file_filter, "All".to_string(), "All Files");
                ui.selectable_value(&mut upload_state.file_filter, "gltf".to_string(), "GLTF Files");
                ui.selectable_value(&mut upload_state.file_filter, "houdini_json".to_string(), "Houdini JSON Files");
            });
        
        ui.separator();
        ui.label("Loaded Models:");
        
        let filtered_models: Vec<_> = state.models.iter()
            .filter(|(_, _, _, file_type)| {
                upload_state.file_filter == "All" || upload_state.file_filter == *file_type
            })
            .collect();
        
        for (id, _path, name, file_type) in &filtered_models {
            let display_name = name
                .as_ref()
                .map_or_else(|| format!("Model {}", id), |n| n.clone());
            let file_type_display = match file_type.as_str() {
                "gltf" => "GLTF",
                "houdini_json" => "Houdini",
                _ => "Unknown"
            };
            
            ui.horizontal(|ui| {
                ui.label(format!("[{}] {}. {}", file_type_display, id, display_name));
                if ui.button("Delete").clicked() {
                    let request = ModelRequest {
                        action: "delete".to_string(),
                        id: Some(*id),
                        name: None,
                        model_data: None,
                        file_type: None,
                        additional_files: None,
                    };
                    let request_str = serde_json::to_string(&request).unwrap();
                    if let Err(e) = upload_state.ws_tx.try_send(request_str) {
                        error!("Failed to send delete request for ID {}: {}", id, e);
                    }
                }
            });
        }
    });

    egui::Window::new("Upload Model")
        .default_pos([1000.0, 50.0])
        .show(contexts.ctx_mut(), |ui| {
            ui.label("Model Name:");
            ui.text_edit_singleline(&mut upload_state.model_name);
            
            ui.separator();
            ui.label("Select a file to upload:");
            
            ui.horizontal(|ui| {
                if ui.button("Choose GLTF File").clicked() {
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
                                    Ok(main_data) => {
                                        let mut additional_files = Vec::new();
                                        
                                        if let Ok(gltf_text) = std::str::from_utf8(&main_data) {
                                            if let Ok(gltf_json) = serde_json::from_str::<serde_json::Value>(gltf_text) {
                                                let parent_dir = path.parent().unwrap();
                                                
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
                                        
                                        (path_str, Ok((main_data, file_name, "gltf".to_string(), additional_files)))
                                    }
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
                
                if ui.button("Choose Houdini JSON").clicked() {
                    if upload_state.status != "Uploading..." {
                        upload_state.status = "Uploading...".to_string();
                        let file_tx = upload_state.file_tx.clone();
                        std::thread::spawn(move || {
                            let (path_str, result) = if let Some(path) = FileDialog::new()
                                .add_filter("JSON Files", &["json"])
                                .pick_file()
                            {
                                let path_str = path.to_string_lossy().to_string();
                                let file_name = Path::new(&path_str)
                                    .file_stem()
                                    .and_then(|stem| stem.to_str())
                                    .map(|s| s.to_string());
                                match std::fs::read(&path) {
                                    Ok(data) => (path_str, Ok((data, file_name, "houdini_json".to_string(), Vec::new()))),
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
            });
            
            ui.separator();
            ui.label(&upload_state.status);
            
            ui.separator();
            ui.label("Supported Formats:");
            ui.label("• GLTF (.gltf) - Standard 3D format");
            ui.label("  (Automatically includes .bin & textures)");
            ui.label("• Houdini JSON (.json) - Geometry from Houdini");
        });

    egui::Window::new("Model Selection")
        .default_pos([640.0, 360.0])
        .show(contexts.ctx_mut(), |ui| {
            let filtered_models: Vec<_> = state.models.iter()
                .filter(|(_, _, _, file_type)| {
                    upload_state.file_filter == "All" || upload_state.file_filter == *file_type
                })
                .collect();
            
            ui.heading("Viewer 1 (Left)");
            let viewer1_selected_text = match upload_state.viewer1_selected_model {
                None => "All Models".to_string(),
                Some(id) => filtered_models
                    .iter()
                    .find(|(model_id, _, _, _)| *model_id == id)
                    .map(|(_, _, name, file_type)| {
                        let file_prefix = match file_type.as_str() {
                            "gltf" => "[GLTF]",
                            "houdini_json" => "[Houdini]",
                            _ => "[Unknown]"
                        };
                        name.as_ref()
                            .map_or_else(|| format!("{} Model {}", file_prefix, id), |n| format!("{} {}: {}", file_prefix, id, n))
                    })
                    .unwrap_or_else(|| "Model Not Found".to_string()),
            };

            egui::ComboBox::from_label("Select Model for Viewer 1")
                .selected_text(viewer1_selected_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut upload_state.viewer1_selected_model, None, "All Models");
                    for (id, _, name, file_type) in &filtered_models {
                        let file_prefix = match file_type.as_str() {
                            "gltf" => "[GLTF]",
                            "houdini_json" => "[Houdini]",
                            _ => "[Unknown]"
                        };
                        let display_name = name
                            .as_ref()
                            .map_or_else(|| format!("{} Model {}", file_prefix, id), |n| format!("{} {}: {}", file_prefix, id, n));
                        ui.selectable_value(&mut upload_state.viewer1_selected_model, Some(*id), display_name);
                    }
                });
            
            ui.separator();
            ui.heading("Viewer 2 (Right)");
            let viewer2_selected_text = match upload_state.viewer2_selected_model {
                None => "All Models".to_string(),
                Some(id) => filtered_models
                    .iter()
                    .find(|(model_id, _, _, _)| *model_id == id)
                    .map(|(_, _, name, file_type)| {
                        let file_prefix = match file_type.as_str() {
                            "gltf" => "[GLTF]",
                            "houdini_json" => "[Houdini]",
                            _ => "[Unknown]"
                        };
                        name.as_ref()
                            .map_or_else(|| format!("{} Model {}", file_prefix, id), |n| format!("{} {}: {}", file_prefix, id, n))
                    })
                    .unwrap_or_else(|| "Model Not Found".to_string()),
            };

            egui::ComboBox::from_label("Select Model for Viewer 2")
                .selected_text(viewer2_selected_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut upload_state.viewer2_selected_model, None, "All Models");
                    for (id, _, name, file_type) in &filtered_models {
                        let file_prefix = match file_type.as_str() {
                            "gltf" => "[GLTF]",
                            "houdini_json" => "[Houdini]",
                            _ => "[Unknown]"
                        };
                        let display_name = name
                            .as_ref()
                            .map_or_else(|| format!("{} Model {}", file_prefix, id), |n| format!("{} {}: {}", file_prefix, id, n));
                        ui.selectable_value(&mut upload_state.viewer2_selected_model, Some(*id), display_name);
                    }
                });
        });
}

fn handle_file_results(mut upload_state: ResMut<UploadState>) {
    while let Ok((path, result)) = upload_state.file_rx.try_recv() {
        match result {
            Ok((data, file_name, file_type, additional_files)) => {
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
                    file_type: Some(file_type),
                    additional_files: if additional_files.is_empty() {
                        None
                    } else {
                        Some(additional_files)
                    },
                };
                let request_str = serde_json::to_string(&request).unwrap();
                if let Err(e) = upload_state.ws_tx.try_send(request_str) {
                    upload_state.status = format!("Failed to queue upload: {}", e);
                    error!("Failed to queue upload: {}", e);
                } else {
                    upload_state.status = "Upload queued".to_string();
                    upload_state.model_name.clear();
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
    let filtered_models: Vec<_> = state.models.iter()
        .filter(|(_, _, _, file_type)| {
            upload_state.file_filter == "All" || upload_state.file_filter == *file_type
        })
        .cloned()
        .collect();
    
    let viewer1_should_update = last_selected.viewer1_id != upload_state.viewer1_selected_model ||
        last_selected.filter != upload_state.file_filter;

    if viewer1_should_update {
        info!("Updating Viewer 1 scene, selected: {:?}, filter: {}", upload_state.viewer1_selected_model, upload_state.file_filter);

        for (_, entity) in state.viewer1_entities.drain(..) {
            info!("Despawning entity for Viewer 1");
            commands.entity(entity).despawn();
        }
        state.viewer1_entities.clear();

        let models_to_load = match upload_state.viewer1_selected_model {
            Some(selected_id) => filtered_models
                .iter()
                .filter(|(id, _, _, _)| *id == selected_id)
                .cloned()
                .collect::<Vec<_>>(),
            None => filtered_models.clone(),
        };

        for (id, temp_path_str, _name, file_type) in models_to_load {
            info!("Loading model ID={} ({}) at path {} for Viewer 1", id, file_type, temp_path_str);
            let entity = commands
                .spawn((
                    SceneRoot(asset_server.load(
                        GltfAssetLabel::Scene(0).from_asset(temp_path_str.clone()),
                    )),
                    ViewerLayer::Viewer1,
                    RenderLayers::layer(0),
                ))
                .id();
            state.viewer1_entities.push((id, entity));
        }

        last_selected.viewer1_id = upload_state.viewer1_selected_model;
    }
    
    let viewer2_should_update = last_selected.viewer2_id != upload_state.viewer2_selected_model ||
        last_selected.filter != upload_state.file_filter;

    if viewer2_should_update {
        info!("Updating Viewer 2 scene, selected: {:?}, filter: {}", upload_state.viewer2_selected_model, upload_state.file_filter);

        for (_, entity) in state.viewer2_entities.drain(..) {
            info!("Despawning entity for Viewer 2");
            commands.entity(entity).despawn();
        }
        state.viewer2_entities.clear();

        let models_to_load = match upload_state.viewer2_selected_model {
            Some(selected_id) => filtered_models
                .iter()
                .filter(|(id, _, _, _)| *id == selected_id)
                .cloned()
                .collect::<Vec<_>>(),
            None => filtered_models.clone(),
        };

        for (id, temp_path_str, _name, file_type) in models_to_load {
            info!("Loading model ID={} ({}) at path {} for Viewer 2", id, file_type, temp_path_str);
            let entity = commands
                .spawn((
                    SceneRoot(asset_server.load(
                        GltfAssetLabel::Scene(0).from_asset(temp_path_str.clone()),
                    )),
                    ViewerLayer::Viewer2,
                    RenderLayers::layer(1),
                ))
                .id();
            state.viewer2_entities.push((id, entity));
        }

        last_selected.viewer2_id = upload_state.viewer2_selected_model;
    }
    
    if last_selected.filter != upload_state.file_filter {
        last_selected.filter = upload_state.file_filter.clone();
    }
}

fn handle_model_updates(
    mut state: ResMut<ModelState>,
    mut receiver: ResMut<ModelUpdateReceiver>,
    mut upload_state: ResMut<UploadState>,
    mut last_selected: ResMut<LastSelectedModel>,
) {
    while let Ok(models) = receiver.0.try_recv() {
        info!("Received {} models, viewer1 selected: {:?}, viewer2 selected: {:?}", 
              models.len(), upload_state.viewer1_selected_model, upload_state.viewer2_selected_model);

        if !models.is_empty() && upload_state.status == "Upload queued" {
            upload_state.status = "Upload successful".to_string();
        }

        let mut new_models = vec![];
        for model in models {
            let temp_path = state
                .models
                .iter()
                .find(|(id, _, _, _)| *id == model.id)
                .map(|(_, path, _, _)| path.clone())
                .unwrap_or_else(|| {
                    let temp_dir = std::env::temp_dir();
                    let model_dir_name = format!("model_{}", model.id);
                    let model_dir = temp_dir.join(&model_dir_name);
                    std::fs::create_dir_all(&model_dir).expect("Failed to create model directory");
                    
                    let temp_file_name = format!("model_{}.gltf", model.id);
                    let temp_path = model_dir.join(&temp_file_name);
                    let temp_path_str = temp_path.to_str().expect("Invalid temp path").to_string();

                    match general_purpose::STANDARD.decode(&model.model_data) {
                        Ok(model_data) => {
                            let mut file = File::create(&temp_path).expect("Failed to create temp file");
                            file.write_all(&model_data).expect("Failed to write temp file");
                            
                            if let Some(additional_files) = &model.additional_files {
                                info!("Writing {} additional files for model ID={}", additional_files.len(), model.id);
                                for add_file in additional_files {
                                    if let Ok(file_data) = general_purpose::STANDARD.decode(&add_file.data) {
                                        let add_path = model_dir.join(&add_file.filename);
                                        if let Some(parent) = add_path.parent() {
                                            std::fs::create_dir_all(parent).ok();
                                        }
                                        if let Ok(mut f) = File::create(&add_path) {
                                            f.write_all(&file_data).ok();
                                            info!("Wrote additional file: {}", add_file.filename);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to decode base64 for model ID={}: {}", model.id, e);
                        }
                    }
                    temp_path_str
                });
            new_models.push((model.id, temp_path, model.name, model.file_type));
        }
        state.models = new_models;

        last_selected.viewer1_id = None;
        last_selected.viewer2_id = None;

        if let Some(selected_id) = upload_state.viewer1_selected_model {
            let filtered_models: Vec<_> = state.models.iter()
                .filter(|(_, _, _, file_type)| {
                    upload_state.file_filter == "All" || upload_state.file_filter == *file_type
                })
                .collect();
            
            if !filtered_models.iter().any(|(id, _, _, _)| *id == selected_id) {
                info!("Viewer 1 selected model ID={} not found in current filter, resetting to All Models", selected_id);
                upload_state.viewer1_selected_model = None;
            }
        }
        
        if let Some(selected_id) = upload_state.viewer2_selected_model {
            let filtered_models: Vec<_> = state.models.iter()
                .filter(|(_, _, _, file_type)| {
                    upload_state.file_filter == "All" || upload_state.file_filter == *file_type
                })
                .collect();
            
            if !filtered_models.iter().any(|(id, _, _, _)| *id == selected_id) {
                info!("Viewer 2 selected model ID={} not found in current filter, resetting to All Models", selected_id);
                upload_state.viewer2_selected_model = None;
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