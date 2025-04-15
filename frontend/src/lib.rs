use bevy::{
    pbr::{ CascadeShadowConfigBuilder, DirectionalLightShadowMap },
    prelude::*,
};
use bevy_panorbit_camera::{ PanOrbitCameraPlugin, PanOrbitCamera };
use bevy_egui::{ egui, EguiContexts, EguiPlugin };
use serde::{ Deserialize, Serialize };
use tokio::sync::mpsc;
use tokio_tungstenite::{ connect_async, tungstenite::Message };
use futures_util::{ SinkExt, StreamExt };
use std::time::Duration;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String,
    id: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModelResponse {
    id: i32,
    path: String,
}

#[derive(Resource)]
struct ModelState {
    models: Vec<ModelResponse>,
    model_entities: Vec<(i32, Entity)>,
}

#[derive(Resource)]
struct ModelUpdateReceiver(mpsc::Receiver<Vec<ModelResponse>>);

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
        .add_systems(Update, (ui_system, handle_model_updates))
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
    commands.insert_resource(ModelUpdateReceiver(update_rx));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        rt.block_on(async {
            let connection_id = Uuid::new_v4().to_string();
            loop {
                info!("Connection {}: Attempting to connect to WebSocket", connection_id);
                match connect_async("ws://127.0.0.1:8000/ws").await {
                    Ok((mut ws_stream, _)) => {
                        info!("Connection {}: WebSocket connected successfully", connection_id);

                        // Send initial get_all request
                        let request = ModelRequest {
                            action: "get_all".to_string(),
                            id: None,
                        };
                        if let Err(e) = ws_stream
                            .send(Message::Text(serde_json::to_string(&request).unwrap().into()))
                            .await
                        {
                            error!("Connection {}: Failed to send initial get_all request: {}", connection_id, e);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                        info!("Connection {}: Sent initial get_all request", connection_id);

                        // Send ping every 10 seconds to keep connection alive
                        let mut ping_interval = tokio::time::interval(Duration::from_secs(10));

                        loop {
                            tokio::select! {
                                Some(message_result) = ws_stream.next() => {
                                    match message_result {
                                        Ok(Message::Text(text)) => {
                                            info!("Connection {}: Received WebSocket message: {}", connection_id, text);
                                            match serde_json::from_str::<Vec<ModelResponse>>(&text) {
                                                Ok(models) => {
                                                    info!("Connection {}: Parsed model list: {:?}", connection_id, models);
                                                    if let Err(e) = update_tx.send(models).await {
                                                        error!("Connection {}: Failed to send models to channel: {}", connection_id, e);
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Connection {}: Failed to parse WebSocket message: {}", connection_id, e);
                                                    warn!("Connection {}: Message content was: {}", connection_id, text);
                                                }
                                            }
                                        }
                                        Ok(Message::Ping(_)) => {
                                            info!("Connection {}: Received ping, sending pong", connection_id);
                                            if let Err(e) = ws_stream.send(Message::Pong(vec![].into())).await {
                                                error!("Connection {}: Failed to send pong: {}", connection_id, e);
                                                break;
                                            }
                                        }
                                        Ok(other) => info!("Connection {}: Received non-text message, ignoring: {:?}", connection_id, other),
                                        Err(e) => {
                                            error!("Connection {}: WebSocket error: {}", connection_id, e);
                                            break;
                                        }
                                    }
                                }
                                _ = ping_interval.tick() => {
                                    info!("Connection {}: Sending ping to keep connection alive", connection_id);
                                    if let Err(e) = ws_stream.send(Message::Ping(vec![].into())).await {
                                        error!("Connection {}: Failed to send ping: {}", connection_id, e);
                                        break;
                                    }
                                }
                            }
                        }
                        info!("Connection {}: WebSocket connection closed, reconnecting in 5 seconds", connection_id);
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
) {
    egui::Window::new("Model List").show(contexts.ctx_mut(), |ui| {
        ui.label("Loaded Models:");
        for model in &state.models {
            ui.label(format!("ID: {} - {}", model.id, model.path));
        }
    });
}

fn handle_model_updates(
    mut state: ResMut<ModelState>,
    mut commands: Commands,
    mut receiver: ResMut<ModelUpdateReceiver>,
    asset_server: Res<AssetServer>,
) {
    while let Ok(models) = receiver.0.try_recv() {
        info!("Received update with models: {:?}", models);

        // Remove entities for models no longer in the list
        state.model_entities.retain(|(id, entity)| {
            if models.iter().any(|m| m.id == *id) {
                true
            } else {
                info!("Removing entity for model ID={}", id);
                commands.entity(*entity).despawn();
                false
            }
        });

        // Load new models into the scene
        for model in &models {
            if !state.model_entities.iter().any(|(id, _)| *id == model.id) {
                info!("Loading new model into scene: ID={}, Path={}", model.id, model.path);
                let entity = commands
                    .spawn(SceneRoot(asset_server.load(
                        GltfAssetLabel::Scene(0).from_asset(model.path.clone()),
                    )))
                    .id();
                state.model_entities.push((model.id, entity));
            }
        }

        // Update the models list
        state.models = models;
        info!("Updated ModelState with {} models", state.models.len());
    }
}

fn debug_resources(world: &World) {
    if world.get_resource::<Assets<Shader>>().is_some() {
        info!("Assets<Shader> resource is available");
    } else {
        error!("Assets<Shader> resource is NOT available");
    }
}