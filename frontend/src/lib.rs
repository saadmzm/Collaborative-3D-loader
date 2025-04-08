use bevy::{
    pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    prelude::*,
};
use bevy_panorbit_camera::{PanOrbitCameraPlugin, PanOrbitCamera};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use futures_util::{SinkExt, StreamExt}; // Import SinkExt and StreamExt

// Message format for WebSocket communication
#[derive(Serialize, Deserialize)]
struct ModelRequest {
    id: i32,
}

#[derive(Serialize, Deserialize)]
struct ModelResponse {
    id: i32,
    path: String,
}

pub fn run() {
    App::new()
        .insert_resource(DirectionalLightShadowMap { size: 4096 })
        .add_plugins(PanOrbitCameraPlugin)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "PGS Renderman".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_systems(Startup, setup)
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

    // Fetch model data via WebSocket
    let model_id = 5; // Replace with desired ID
    match fetch_model_by_id(model_id) {
        Ok(model) => {
            commands.spawn(SceneRoot(asset_server.load(
                GltfAssetLabel::Scene(0).from_asset(model.path),
            )));
        }
        Err(e) => {
            error!("Failed to fetch model from backend: {:?}", e);
            commands.spawn(SceneRoot(asset_server.load(
                GltfAssetLabel::Scene(0).from_asset("models/tree.gltf"),
            )));
        }
    }
}

// Function to fetch model data via WebSocket
fn fetch_model_by_id(model_id: i32) -> Result<ModelResponse, String> {
    // Create a Tokio runtime with explicit configuration
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create runtime: {}", e))?;

    rt.block_on(async {
        // Connect to the WebSocket server
        let (mut ws_stream, _) = connect_async("ws://17.76.57.76:8000/ws")
            .await
            .map_err(|e| format!("Failed to connect to WebSocket: {}", e))?;

        // Send model request
        let request = ModelRequest { id: model_id };
        ws_stream
            .send(Message::Text(
                serde_json::to_string(&request).map_err(|e| format!("Failed to serialize request: {}", e))?,
            ))
            .await
            .map_err(|e| format!("Failed to send request: {}", e))?;

        // Receive response
        if let Some(message) = ws_stream
            .next()
            .await
            .transpose()
            .map_err(|e| format!("Failed to receive response: {}", e))?
        {
            if let Message::Text(text) = message {
                let response: ModelResponse = serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to deserialize response: {}", e))?;
                return Ok(response);
            }
        }
        Err("No valid response received".to_string())
    })
}