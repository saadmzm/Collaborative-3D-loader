use bevy::{
    pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    prelude::*,
};
use bevy_panorbit_camera::{PanOrbitCameraPlugin, PanOrbitCamera};
use rusqlite::{params, Connection, Result};

// Define a struct to hold model data from the database
#[derive(Debug)]
struct ModelData {
    path: String, // Assuming the database stores the file path; adjust if it stores binary data
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
    // Spawn the camera and light as before
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

    // Database integration: Load model path from SQLite
    match load_model_from_db() {
        Ok(model_data) => {
            // Spawn the scene using the path retrieved from the database
            commands.spawn(SceneRoot(asset_server.load(
                GltfAssetLabel::Scene(0).from_asset(model_data.path),
            )));
        }
        Err(e) => {
            error!("Failed to load model from database: {:?}", e);
            // Fallback: Load a default model if database fails
            commands.spawn(SceneRoot(asset_server.load(
                GltfAssetLabel::Scene(0).from_asset("models/tree.gltf"),
            )));
        }
    }
}

// Function to load model data from the database
fn load_model_from_db() -> Result<ModelData> {
    // Open or create the SQLite database
    let conn = Connection::open("models.db")?;

    // Create a table to store model paths if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL
        )",
        params![],
    )?;

    // For this example, assume we fetch the first model (you can adjust the query as needed)
    let mut stmt = conn.prepare("SELECT path FROM models LIMIT 1")?;
    let model_data = stmt.query_row(params![], |row| {
        Ok(ModelData {
            path: row.get(0)?,
        })
    })?;

    Ok(model_data)
}