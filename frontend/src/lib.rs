use bevy::{
    pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    prelude::*,
};
use bevy_panorbit_camera::{PanOrbitCameraPlugin, PanOrbitCamera};
use bevy_egui::{egui, EguiContexts, EguiPlugin};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};

// Message formats for WebSocket communication
#[derive(Serialize, Deserialize)]
struct ModelRequest {
    action: String, // "get_by_id" or "get_all"
    id: Option<i32>, // Used for get_by_id
}

#[derive(Serialize, Deserialize, Clone)]
struct ModelResponse {
    id: i32,
    path: String,
}

// Resource to store available models and selected model
#[derive(Resource)]
struct ModelState {
    models: Vec<ModelResponse>,
    selected_model_id: Option<i32>,
    current_entity: Option<Entity>,
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
        .add_systems(Update, ui_system)
        .add_systems(Startup, debug_resources)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Spawn the camera and light
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

    // Fetch all models and initialize state
    match fetch_all_models() {
        Ok(models) => {
            let initial_model = models.get(0).cloned();
            let initial_entity = initial_model.clone().map(|model| {
                commands
                    .spawn(SceneRoot(asset_server.load(
                        GltfAssetLabel::Scene(0).from_asset(model.path.clone()),
                    )))
                    .id()
            });
            commands.insert_resource(ModelState {
                models,
                selected_model_id: initial_model.map(|m| m.id),
                current_entity: initial_entity,
            });
        }
        Err(e) => {
            error!("Failed to fetch models from backend: {:?}", e);
            let entity = commands
                .spawn(SceneRoot(asset_server.load(
                    GltfAssetLabel::Scene(0).from_asset("models/tree.gltf"),
                )))
                .id();
            commands.insert_resource(ModelState {
                models: vec![],
                selected_model_id: None,
                current_entity: Some(entity),
            });
        }
    }
}

fn ui_system(
    mut contexts: EguiContexts,
    mut state: ResMut<ModelState>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
) {
    egui::Window::new("Model Selector").show(contexts.ctx_mut(), |ui| {
        let selected_id = state.selected_model_id.unwrap_or(-1); // -1 for "None"
        let mut new_selected_id = selected_id;

        egui::ComboBox::from_label("Select Model")
            .selected_text(
                state
                    .models
                    .iter()
                    .find(|m| m.id == selected_id)
                    .map_or("None".to_string(), |m| format!("ID: {} - {}", m.id, m.path)),
            )
            .show_ui(ui, |ui| {
                for model in &state.models {
                    ui.selectable_value(
                        &mut new_selected_id,
                        model.id,
                        format!("ID: {} - {}", model.id, model.path), // Fixed: m.path -> model.path
                    );
                }
            });

        // Update model if selection changed
        if new_selected_id != selected_id {
            state.selected_model_id = Some(new_selected_id);

            // Despawn the current model if it exists
            if let Some(entity) = state.current_entity {
                commands.entity(entity).despawn();
            }

            // Spawn the new model
            if let Some(model) = state.models.iter().find(|m| m.id == new_selected_id) {
                let new_entity = commands
                    .spawn(SceneRoot(asset_server.load(
                        GltfAssetLabel::Scene(0).from_asset(model.path.clone()),
                    )))
                    .id();
                state.current_entity = Some(new_entity);
            }
        }
    });
}

// Debug system to check resource availability
fn debug_resources(world: &World) {
    if world.get_resource::<Assets<Shader>>().is_some() {
        info!("Assets<Shader> resource is available");
    } else {
        error!("Assets<Shader> resource is NOT available");
    }
}

// Fetch all models from the backend
fn fetch_all_models() -> Result<Vec<ModelResponse>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create runtime: {}", e))?;

    rt.block_on(async {
        let (mut ws_stream, _) = connect_async("ws://127.0.0.1:8000/ws")
            .await
            .map_err(|e| format!("Failed to connect to WebSocket: {}", e))?;

        // Send request for all models
        let request = ModelRequest {
            action: "get_all".to_string(),
            id: None,
        };
        ws_stream
            .send(Message::Text(
                serde_json::to_string(&request).map_err(|e| format!("Failed to serialize request: {}", e))?,
            ))
            .await
            .map_err(|e| format!("Failed to send request:fonso {}", e))?;

        // Receive response
        if let Some(message) = ws_stream
            .next()
            .await
            .transpose()
            .map_err(|e| format!("Failed to receive response: {}", e))?
        {
            if let Message::Text(text) = message {
                let response: Vec<ModelResponse> = serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to deserialize response: {}", e))?;
                return Ok(response);
            }
        }
        Err("No valid response received".to_string())
    })
}