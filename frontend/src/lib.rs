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
    selected_model_id: Option<i32>,
    current_entity: Option<Entity>,
}

#[derive(Resource)]
struct ModelUpdateReceiver(mpsc::Receiver<Vec<ModelResponse>>);

#[derive(Resource)]
struct WebSocketCommandSender(mpsc::Sender<String>);

#[derive(Resource)]
struct RefreshTimer(Timer);

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
        selected_model_id: None,
        current_entity: None,
    });

    commands.insert_resource(RefreshTimer(Timer::new(
        std::time::Duration::from_secs_f32(1.0 / 30.0),
        TimerMode::Repeating,
    )));

    let (update_tx, update_rx) = mpsc::channel(16);
    let (command_tx, mut command_rx) = mpsc::channel(16);
    commands.insert_resource(ModelUpdateReceiver(update_rx));
    commands.insert_resource(WebSocketCommandSender(command_tx));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        rt.block_on(async {
            info!("Starting WebSocket listener thread");
            match connect_async("ws://127.0.0.1:8000/ws").await {
                Ok((mut ws_stream, _)) => {
                    info!("WebSocket connected successfully");

                    let request = ModelRequest {
                        action: "get_all".to_string(),
                        id: None,
                    };
                    if let Err(e) = ws_stream.send(Message::Text(serde_json::to_string(&request).unwrap().into())).await {
                        error!("Failed to send initial get_all request: {}", e);
                        return;
                    }
                    info!("Sent initial get_all request");

                    loop {
                        tokio::select! {
                            Some(message_result) = ws_stream.next() => {
                                match message_result {
                                    Ok(Message::Text(text)) => {
                                        info!("Received WebSocket message: {}", text);
                                        if let Ok(models) = serde_json::from_str::<Vec<ModelResponse>>(&text) {
                                            info!("Parsed model list: {:?}", models);
                                            if let Err(e) = update_tx.send(models).await {
                                                error!("Failed to send models to channel: {}", e);
                                                break;
                                            }
                                        } else if let Ok(model) = serde_json::from_str::<ModelResponse>(&text) {
                                            info!("Parsed single model: ID={}, Path={}", model.id, model.path);
                                            if let Err(e) = update_tx.send(vec![model]).await {
                                                error!("Failed to send single model to channel: {}", e);
                                                break;
                                            }
                                        } else {
                                            warn!("Failed to parse WebSocket message: {}", text);
                                        }
                                    }
                                    Ok(_) => info!("Received non-text message, ignoring"),
                                    Err(e) => {
                                        error!("WebSocket error: {}", e);
                                        break;
                                    }
                                }
                            }
                            Some(command) = command_rx.recv() => {
                                info!("Received command from UI: {}", command);
                                if let Err(e) = ws_stream.send(Message::Text(command.into())).await {
                                    error!("Failed to send command to WebSocket: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                    error!("WebSocket connection closed unexpectedly");
                }
                Err(e) => error!("Failed to connect to WebSocket: {}", e),
            }
        });
    });
}

fn ui_system(
    mut contexts: EguiContexts,
    mut state: ResMut<ModelState>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    command_sender: Res<WebSocketCommandSender>,
    time: Res<Time>,
    mut refresh_timer: ResMut<RefreshTimer>,
) {
    egui::Window::new("Model Selector").show(contexts.ctx_mut(), |ui| {
        let selected_id = state.selected_model_id.unwrap_or(-1);
        let mut new_selected_id = selected_id;

        ui.horizontal(|ui| {
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
                            format!("ID: {} - {}", model.id, model.path),
                        );
                    }
                });
        });

        refresh_timer.0.tick(time.delta());
        if refresh_timer.0.just_finished() {
            info!("Automatic refresh triggered (30 Hz), sending get_all request");
            send_get_all_request(&command_sender);
        }

        if new_selected_id != selected_id {
            state.selected_model_id = Some(new_selected_id);
            if let Some(entity) = state.current_entity {
                commands.entity(entity).despawn();
            }
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

fn send_get_all_request(command_sender: &WebSocketCommandSender) {
    let request = ModelRequest {
        action: "get_all".to_string(),
        id: None,
    };
    let request_str = serde_json::to_string(&request).unwrap();
    if let Err(e) = command_sender.0.try_send(request_str) {
        error!("Failed to send get_all request: {}", e);
    }
}

fn handle_model_updates(mut state: ResMut<ModelState>, mut receiver: ResMut<ModelUpdateReceiver>) {
    while let Ok(models) = receiver.0.try_recv() {
        info!("Updating ModelState with: {:?}", models);
        state.models = models;
    }
}

fn debug_resources(world: &World) {
    if world.get_resource::<Assets<Shader>>().is_some() {
        info!("Assets<Shader> resource is available");
    } else {
        error!("Assets<Shader> resource is NOT available");
    }
}