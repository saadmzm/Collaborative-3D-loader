use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use std::path::PathBuf;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .init_resource::<CurrentModel>()
        .add_systems(Startup, setup)
        .add_systems(Update, handle_drop_files)
        .run();
}

#[derive(Resource, Default)]
struct CurrentModel {
    entity: Option<Entity>,
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    // Camera
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(0.0, 2.0, 5.0)
            .looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Light
    commands.spawn(PointLightBundle {
        point_light: PointLight {
            intensity: 1500.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(4.0, 8.0, 4.0),
        ..default()
    });

    // Initial model (optional)
    commands.spawn(SceneBundle {
        scene: bevy::prelude::SceneRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset("models/tree.gltf"))),
        transform: Transform::from_xyz(0.0, 0.0, 0.0),
        ..default()
    });

    // Plane
    commands.spawn(PbrBundle {
        mesh: bevy::prelude::Mesh3d(meshes.add(Plane3d::default().mesh().size(10.0, 10.0))),
        material: bevy::prelude::MeshMaterial3d(materials.add(Color::rgb(0.3, 0.5, 0.3))),
        ..default()
    });
}

fn handle_drop_files(
    mut commands: Commands,
    mut drop_events: EventReader<FileDragAndDrop>,
    mut current_model: ResMut<CurrentModel>,
    asset_server: Res<AssetServer>,
) {
    for event in drop_events.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = event {
            let extension = path_buf.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase());

            if extension == Some("gltf".to_string()) || extension == Some("glb".to_string()) {
                info!("Attempting to load GLTF from: {:?}", path_buf);

                let path_str = match path_buf.to_str() {
                    Some(path) => path,
                    None => {
                        error!("Invalid UTF-8 path: {:?}", path_buf);
                        continue;
                    }
                };

                // Remove previous model if it exists
                if let Some(entity) = current_model.entity {
                    commands.entity(entity).despawn_recursive();
                }

                // Load GLTF using GltfAssetLabel with the absolute path
                let gltf_path = PathBuf::from(path_str);
                let model_handle = asset_server.load(GltfAssetLabel::Scene(0).from_asset(gltf_path));
                
                let new_entity = commands.spawn(SceneBundle {
                    scene: bevy::prelude::SceneRoot(model_handle),
                    transform: Transform::from_xyz(0.0, 0.0, 0.0),
                    ..default()
                }).id();

                current_model.entity = Some(new_entity);
                info!("GLTF model load initiated from dropped file");
            } else {
                warn!("Dropped file is not a GLTF/GLB: {:?}", path_buf);
            }
        }
    }
}