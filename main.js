import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

// Scene setup
const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.1, 1000);
const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setSize(window.innerWidth, window.innerHeight);
document.body.appendChild(renderer.domElement);

// Lighting
const ambientLight = new THREE.AmbientLight(0xffffff, 0.8);
scene.add(ambientLight);
const directionalLight = new THREE.DirectionalLight(0xffffff, 0.5);
directionalLight.position.set(5, 5, 5);
scene.add(directionalLight);
const pointLight = new THREE.PointLight(0xffffff, 1, 100);
pointLight.position.set(0, 5, 5);
scene.add(pointLight);
const spotLight = new THREE.SpotLight(0xffffff, 1, 50, Math.PI / 6, 0.5);
spotLight.position.set(5, 10, 5);
spotLight.target.position.set(0, 0, 0);
scene.add(spotLight);
scene.add(spotLight.target);

// Camera position
camera.position.set(0, 2, 5);

// Orbit controls
const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;
controls.dampingFactor = 0.05;

// glTF loader
const loader = new GLTFLoader();
let currentModels = [];

// WebSocket setup
const ws = new WebSocket('ws://127.0.0.1:8000/ws');
const statusDiv = document.getElementById('status');
const modelSelect = document.getElementById('modelSelect');
let requestTimeout = null;
let allModels = [];

ws.onopen = () => {
    console.log('WebSocket connected');
    statusDiv.textContent = 'Connected to WebSocket';
    statusDiv.style.color = 'green';
    // Request all models on connection
    const getAllRequest = { action: 'get_all' };
    console.log('Sending get_all request:', getAllRequest);
    ws.send(JSON.stringify(getAllRequest));
};

ws.onclose = () => {
    console.log('WebSocket disconnected');
    statusDiv.textContent = 'Disconnected from WebSocket';
    statusDiv.style.color = 'red';
    clearTimeout(requestTimeout);
};

ws.onerror = (error) => {
    console.error('WebSocket error:', error);
    statusDiv.textContent = 'WebSocket error';
    statusDiv.style.color = 'red';
    clearTimeout(requestTimeout);
};

ws.onmessage = (event) => {
    console.log('Received WebSocket message, type:', typeof event.data, 'data:', event.data);
    clearTimeout(requestTimeout);
    if (typeof event.data !== 'string') {
        console.error('Expected string message, got:', typeof event.data);
        statusDiv.textContent = 'Unexpected message type from server';
        statusDiv.style.color = 'red';
        return;
    }
    try {
        const data = JSON.parse(event.data);
        console.log('Parsed response:', data);
        if (data.error) {
            console.log('Server error:', data.error);
            statusDiv.textContent = `Error: ${data.error}`;
            statusDiv.style.color = 'red';
        } else if (data.models && Array.isArray(data.models)) {
            // Handle new model notification with model list update
            console.log('Received model list with new model notification:', data);
            allModels = data.models;
            updateModelSelect(data.models);
            if (data.new_model_id) {
                console.log('New model added with ID:', data.new_model_id);
            }
            // Update scene based on current selection
            updateScene();
        } else if (Array.isArray(data)) {
            // Handle get_all response (plain array)
            console.log('Received model list:', data);
            allModels = data;
            updateModelSelect(data);
            // Update scene based on current selection
            updateScene();
        } else if (data.id && data.model_data) {
            // Handle get_by_id response
            console.log('Received model:', data);
            // Check if we're loading all models or just one
            const isLoadingAll = modelSelect.value === 'all';
            loadModelFromResponse([data], !isLoadingAll); // Only clear scene if not loading all
        } else {
            console.log('Unexpected response format:', data);
            statusDiv.textContent = 'Unexpected server response';
            statusDiv.style.color = 'red';
        }
    } catch (e) {
        console.error('Parse error:', e, 'Raw message:', event.data);
        statusDiv.textContent = 'Invalid response from server';
        statusDiv.style.color = 'red';
    }
};

function updateModelSelect(models) {
    console.log('Updating dropdown with models:', models);
    const currentSelection = modelSelect.value;
    modelSelect.innerHTML = '<option value="">Select a model</option><option value="all">Load All Models</option>';
    if (models.length === 0) {
        statusDiv.textContent = 'No models available in database';
        statusDiv.style.color = 'orange';
        console.log('Model list is empty');
        if (currentSelection === '' || currentSelection === 'all') {
            modelSelect.value = currentSelection;
        }
        return;
    }
    models.forEach((model) => {
        // Model list items only have id, name, and file_type (no model_data)
        if (model.id) {
            const option = document.createElement('option');
            option.value = model.id;
            const fileType = model.file_type ? `[${model.file_type.toUpperCase()}]` : '';
            option.textContent = model.name ? `${fileType} ${model.name} (ID: ${model.id})` : `${fileType} Model ID: ${model.id}`;
            modelSelect.appendChild(option);
        } else {
            console.log('Skipping invalid model:', model);
        }
    });
    if (currentSelection && currentSelection !== 'all') {
        const optionExists = Array.from(modelSelect.options).some(option => option.value === currentSelection);
        modelSelect.value = optionExists ? currentSelection : '';
    } else {
        modelSelect.value = currentSelection || '';
    }
    statusDiv.textContent = `Model list updated (${models.length} models)`;
    statusDiv.style.color = 'green';
}

function loadModelFromResponse(models, clearScene = false) {
    console.log('Loading models:', models);
    if (clearScene) {
        currentModels.forEach(model => scene.remove(model));
        currentModels = [];
    }

    let loadedCount = 0;
    const totalModels = models.length;

    models.forEach((model, index) => {
        try {
            const binaryString = atob(model.model_data);
            const len = binaryString.length;
            const bytes = new Uint8Array(len);
            for (let i = 0; i < len; i++) {
                bytes[i] = binaryString.charCodeAt(i);
            }
            
            // Create blob with proper MIME type
            const mimeType = model.file_type === 'glb' ? 'model/gltf-binary' : 'model/gltf+json';
            const blob = new Blob([bytes], { type: mimeType });
            const url = URL.createObjectURL(blob);
            
            // Use load() instead of parse() - this properly handles embedded resources
            loader.load(
                url,
                (gltf) => {
                    const modelScene = gltf.scene;
                    modelScene.position.set(index * 3, 0, 0);
                    scene.add(modelScene);
                    currentModels.push(modelScene);
                    loadedCount++;
                    
                    // Clean up blob URL
                    URL.revokeObjectURL(url);

                    const box = new THREE.Box3();
                    currentModels.forEach(m => box.expandByObject(m));
                    const center = box.getCenter(new THREE.Vector3());
                    const size = box.getSize(new THREE.Vector3());
                    const maxDim = Math.max(size.x, size.y, size.z, 5);
                    camera.position.set(center.x, center.y, center.z + maxDim * 2);
                    controls.target = center;
                    spotLight.target.position.copy(center);

                    if (loadedCount === totalModels) {
                        statusDiv.textContent = `Loaded ${totalModels} model${totalModels > 1 ? 's' : ''}`;
                        statusDiv.style.color = 'green';
                        console.log('All models loaded successfully');
                    }
                },
                undefined,
                (error) => {
                    statusDiv.textContent = `Error loading model ID: ${model.id}`;
                    statusDiv.style.color = 'red';
                    console.error(`Error loading glTF/GLB for ID ${model.id}:`, error);
                    
                    // Clean up blob URL on error
                    URL.revokeObjectURL(url);
                    
                    loadedCount++;
                    if (loadedCount === totalModels) {
                        statusDiv.textContent = `Loaded ${totalModels} model${totalModels > 1 ? 's' : ''} with errors`;
                        statusDiv.style.color = 'orange';
                    }
                }
            );
        } catch (e) {
            statusDiv.textContent = `Error decoding model data for ID: ${model.id}`;
            statusDiv.style.color = 'red';
            console.error(`Base64 decode error for ID ${model.id}:`, e);
            loadedCount++;
            if (loadedCount === totalModels) {
                statusDiv.textContent = `Loaded ${totalModels} model${totalModels > 1 ? 's' : ''} with errors`;
                statusDiv.style.color = 'orange';
            }
        }
    });
}

function updateScene() {
    const modelId = modelSelect.value;
    if (modelId === 'all') {
        if (allModels.length === 0) {
            statusDiv.textContent = 'No models available to load';
            statusDiv.style.color = 'red';
            console.log('No models in allModels');
            currentModels.forEach(model => scene.remove(model));
            currentModels = [];
            return;
        }
        console.log('Requesting all models:', allModels);
        statusDiv.textContent = `Requesting all ${allModels.length} models...`;
        
        // Clear current models
        currentModels.forEach(model => scene.remove(model));
        currentModels = [];
        
        // Request each model individually
        allModels.forEach((modelItem, index) => {
            if (ws.readyState === WebSocket.OPEN) {
                const getByIdRequest = { action: 'get_by_id', id: modelItem.id };
                console.log(`Requesting model ${index + 1}/${allModels.length}, ID:`, modelItem.id);
                ws.send(JSON.stringify(getByIdRequest));
            }
        });
    } else if (modelId) {
        const modelIdNum = parseInt(modelId);
        if (!isNaN(modelIdNum) && modelIdNum > 0 && ws.readyState === WebSocket.OPEN) {
            // Check if the model still exists in allModels
            const modelExists = allModels.some(model => model.id === modelIdNum);
            if (!modelExists) {
                statusDiv.textContent = 'Selected model no longer exists';
                statusDiv.style.color = 'red';
                modelSelect.value = '';
                currentModels.forEach(model => scene.remove(model));
                currentModels = [];
                return;
            }
            const getByIdRequest = { action: 'get_by_id', id: modelIdNum };
            console.log('Sending get_by_id request:', getByIdRequest);
            ws.send(JSON.stringify(getByIdRequest));
            statusDiv.textContent = 'Requesting model...';
            requestTimeout = setTimeout(() => {
                statusDiv.textContent = 'No response from server';
                statusDiv.style.color = 'red';
                console.log('Request timed out for ID:', modelIdNum);
            }, 5000);
        }
    } else {
        // Clear scene if "Select a model" is chosen
        currentModels.forEach(model => scene.remove(model));
        currentModels = [];
        statusDiv.textContent = 'No model selected';
        statusDiv.style.color = 'orange';
    }
}

modelSelect.addEventListener('change', () => {
    console.log('Dropdown selection changed to:', modelSelect.value);
    updateScene();
});

window.addEventListener('resize', () => {
    camera.aspect = window.innerWidth / window.innerHeight;
    camera.updateProjectionMatrix();
    renderer.setSize(window.innerWidth, window.innerHeight);
});

function animate() {
    requestAnimationFrame(animate);
    controls.update();
    renderer.render(scene, camera);
}
animate();