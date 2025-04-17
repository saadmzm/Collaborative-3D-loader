use bevy::{
    asset::{AssetLoader, Handle, LoadContext},
    pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    prelude::*,
    utils::HashMap,
};
use bevy_egui::{egui, EguiContexts, EguiPlugin};
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::{FileReader, HtmlInputElement, WebSocket, MessageEvent, Event};
use js_sys::{JsString, Uint8Array};
use base64::Engine;
use std::future::Future;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref WEBSOCKET_MESSAGE_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static ref PENDING_UPLOADS_BUFFER: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
}

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
    models: Vec<(i32, Vec<u8>, Option<String>)>, // (id, model_data, name)
    model_entities: Vec<(i32, Entity)>,
    model_handles: HashMap<i32, Handle<Scene>>,
}

#[derive(Resource)]
struct LastSelectedModel {
    id: Option<i32>,
}

struct WebSocketWrapper(WebSocket);

unsafe impl Send for WebSocketWrapper {}
unsafe impl Sync for WebSocketWrapper {}

#[derive(Resource)]
struct WebSocketState {
    ws: WebSocketWrapper,
    pending_uploads: Vec<(String, Vec<u8>)>,
    selected_model: Option<i32>,
}

pub fn run() {
    App::new()
        .insert_resource(DirectionalLightShadowMap { size: 4096 })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "PGS Renderman".to_string(),
                        canvas: Some("#bevy-canvas".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .set(AssetPlugin {
                    file_path: "".to_string(),
                    ..Default::default()
                }),
        )
        .add_plugins(PanOrbitCameraPlugin)
        .add_plugins(EguiPlugin)
        .init_asset_loader::<GltfMemoryLoader>()
        .add_systems(Startup, setup)
        .add_systems(Update, (
            setup_websocket,
            process_websocket_messages,
            ui_system,
            handle_file_uploads,
            update_scene_on_selection,
            block_camera_on_egui,
        ))
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
        model_handles: HashMap::new(),
    });

    commands.insert_resource(LastSelectedModel { id: None });

    let ws = WebSocket::new("ws://127.0.0.1:8000/ws").expect("Failed to create WebSocket");
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
    commands.insert_resource(WebSocketState {
        ws: WebSocketWrapper(ws),
        pending_uploads: vec![],
        selected_model: None,
    });
}

fn setup_websocket(ws_state: Res<WebSocketState>) {
    let ws = ws_state.ws.0.clone();
    let initial_request = ModelRequest {
        action: "get_all".to_string(),
        id: None,
        name: None,
        model_data: None,
    };
    let request_str = serde_json::to_string(&initial_request).unwrap();
    ws.send_with_str(&request_str).unwrap();

    let closure = Closure::wrap(Box::new(move |event: MessageEvent| {
        if let Ok(text) = event.data().dyn_into::<JsString>() {
            let text: String = text.into();
            if let Ok(mut buffer) = WEBSOCKET_MESSAGE_BUFFER.lock() {
                buffer.push(text);
            }
        }
    }) as Box<dyn FnMut(_)>);
    ws.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
}

fn process_websocket_messages(
    mut state: ResMut<ModelState>,
) {
    let messages: Vec<String> = {
        if let Ok(mut buffer) = WEBSOCKET_MESSAGE_BUFFER.lock() {
            std::mem::take(&mut *buffer)
        } else {
            vec![]
        }
    };

    for text in messages {
        if let Ok(models) = serde_json::from_str::<Vec<ModelResponse>>(&text) {
            let mut new_models = vec![];
            for model in models {
                if let Ok(model_data) = base64::engine::general_purpose::STANDARD.decode(&model.model_data) {
                    new_models.push((model.id, model_data, model.name.clone()));
                }
            }
            state.models = new_models;
        }
    }
}

fn ui_system(
    mut contexts: EguiContexts,
    state: Res<ModelState>,
    mut ws_state: ResMut<WebSocketState>,
) {
    egui::Window::new("Model List").show(contexts.ctx_mut(), |ui| {
        ui.label("Loaded Models:");
        for (id, _, name) in &state.models {
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
                    ws_state.ws.0.send_with_str(&request_str).unwrap();
                }
            });
        }
    });

    egui::Window::new("Upload Model")
        .default_pos([1000.0, 50.0])
        .show(contexts.ctx_mut(), |ui| {
            ui.label("Select a .gltf file to upload:");
            if ui.button("Choose File").clicked() {
                if let Some(input) = web_sys::window()
                    .and_then(|win| win.document())
                    .and_then(|doc| doc.get_element_by_id("file-input"))
                    .and_then(|elem| elem.dyn_into::<HtmlInputElement>().ok())
                {
                    input.click();
                }
            }
        });

    egui::Window::new("Model Selection")
        .default_pos([640.0, 360.0])
        .show(contexts.ctx_mut(), |ui| {
            let selected_text = match ws_state.selected_model {
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
                    ui.selectable_value(&mut ws_state.selected_model, None, "All Models");
                    for (id, _, name) in &state.models {
                        let display_name = name
                            .as_ref()
                            .map_or_else(|| format!("Model {}", id), |n| format!("{}: {}", id, n));
                        ui.selectable_value(&mut ws_state.selected_model, Some(*id), display_name);
                    }
                });
        });
}

fn handle_file_uploads(
    ws_state: Res<WebSocketState>,
    mut state: ResMut<WebSocketState>,
) {
    let ws = ws_state.ws.0.clone();
    if let Some(input) = web_sys::window()
        .and_then(|win| win.document())
        .and_then(|doc| doc.get_element_by_id("file-input"))
        .and_then(|elem| elem.dyn_into::<HtmlInputElement>().ok())
    {
        let input_clone = input.clone();
        let ws_clone = ws.clone();
        let closure = Closure::wrap(Box::new(move |_: Event| {
            if let Some(file_list) = input_clone.files() {
                if let Some(file) = file_list.get(0) {
                    let reader = FileReader::new().unwrap();
                    let reader_clone = reader.clone();
                    let ws_clone2 = ws_clone.clone();
                    let file_name = file.name();
                    let name = if file_name.ends_with(".gltf") {
                        Some(file_name.strip_suffix(".gltf").unwrap_or(&file_name).to_string())
                    } else {
                        None
                    };
                    let file_name_clone = file_name.clone();
                    let onload = Closure::wrap(Box::new(move |_: Event| {
                        if let Ok(buffer) = reader_clone.result() {
                            let array = Uint8Array::new(&buffer);
                            let data = array.to_vec();
                            let base64_data = base64::engine::general_purpose::STANDARD.encode(&data);
                            let request = ModelRequest {
                                action: "insert".to_string(),
                                id: None,
                                name: name.clone(),
                                model_data: Some(base64_data),
                            };
                            let request_str = serde_json::to_string(&request).unwrap();
                            ws_clone2.send_with_str(&request_str).unwrap();
                            if let Ok(mut uploads) = PENDING_UPLOADS_BUFFER.lock() {
                                uploads.push((file_name_clone.clone(), data));
                            }
                            web_sys::console::log_1(&format!("Uploaded: {}", file_name_clone).into());
                        }
                    }) as Box<dyn FnMut(_)>);
                    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                    reader.read_as_array_buffer(&file).unwrap();
                    onload.forget();
                }
            }
        }) as Box<dyn FnMut(_)>);
        input.set_onchange(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
    }

    // Sync static buffer to state
    if let Ok(mut uploads) = PENDING_UPLOADS_BUFFER.lock() {
        state.pending_uploads.extend(std::mem::take(&mut *uploads));
    }
}

fn update_scene_on_selection(
    mut commands: Commands,
    mut state: ResMut<ModelState>,
    mut last_selected: ResMut<LastSelectedModel>,
    ws_state: Res<WebSocketState>,
    mut assets: ResMut<Assets<Scene>>,
) {
    let should_update = last_selected.id != ws_state.selected_model ||
        state.model_entities.iter().map(|(id, _)| *id).collect::<Vec<_>>() !=
        match ws_state.selected_model {
            Some(id) => state.models.iter().filter(|(mid, _, _)| *mid == id).map(|(id, _, _)| *id).collect::<Vec<_>>(),
            None => state.models.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        };

    if should_update {
        info!("Updating scene");

        for (_, entity) in state.model_entities.drain(..) {
            commands.entity(entity).despawn();
        }
        state.model_entities.clear();

        let filtered_models = match ws_state.selected_model {
            Some(selected_id) => state.models.iter().filter(|(id, _, _)| *id == selected_id).cloned().collect::<Vec<_>>(),
            None => state.models.clone(),
        };

        for (id, _model_data, _name) in filtered_models {
            if let Some(handle) = state.model_handles.get(&id) {
                let entity = commands.spawn(SceneRoot(handle.clone())).id();
                state.model_entities.push((id, entity));
            } else {
                let world = World::new();
                let handle = assets.add(Scene::new(world));
                state.model_handles.insert(id, handle.clone());
                let entity = commands.spawn(SceneRoot(handle)).id();
                state.model_entities.push((id, entity));
            }
        }

        last_selected.id = ws_state.selected_model;
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

#[derive(Default)]
struct GltfMemoryLoader;

impl AssetLoader for GltfMemoryLoader {
    type Asset = Scene;
    type Settings = ();
    type Error = bevy::asset::AssetLoaderError;

    fn load<'a, 'b>(
        &'a self,
        reader: &'b mut dyn bevy::asset::io::Reader,
        _settings: &Self::Settings,
        load_context: &mut LoadContext,
    ) -> impl Future<Output = Result<Self::Asset, Self::Error>> {
        async move {
            let mut bytes = Vec::new();
            if let Err(_) = reader.read_to_end(&mut bytes).await {
                // Create a generic boxed error
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other, 
                    "Failed to read asset"
                )) as Box<dyn std::error::Error>);
            }
            
            let world = World::new();
            let scene = Scene::new(world);
            load_context.add_labeled_asset("scene".to_string(), scene);
            Ok(scene)
        }
    }

    fn extensions(&self) -> &[&str] {
        &["gltf"]
    }
}
