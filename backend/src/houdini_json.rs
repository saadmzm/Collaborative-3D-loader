use serde::{Deserialize, Serialize};
use base64::Engine;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HoudiniGeometryInfo {
    pub date: String,
    pub timetocook: i32,
    pub software: String,
    pub artist: String,
    pub hostname: String,
    pub time: i32,
    pub bounds: Vec<f32>, // [xmin, xmax, ymin, ymax, zmin, zmax]
    pub primcount_summary: String,
    pub attribute_summary: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct BevyGeometry {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub normals: Vec<[f32; 3]>,
    pub primitives: Vec<BevyPrimitive>,
    pub bounds: [f32; 6], // [xmin, xmax, ymin, ymax, zmin, zmax]
    pub metadata: GeometryMetadata,
}

#[derive(Debug, Serialize, Clone)]
pub struct BevyPrimitive {
    pub primitive_type: String, // "LineList", "TriangleList", etc.
    pub start_index: usize,
    pub count: usize,
    pub material_id: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct GeometryMetadata {
    pub source_software: String,
    pub artist: String,
    pub feature_id: Option<String>,
    pub input_name: Option<String>,
    pub creation_date: String,
}

pub struct HoudiniJsonParser;

impl HoudiniJsonParser {
    pub fn parse_from_json(json_content: &str) -> Result<BevyGeometry, String> {
        // Parse the Houdini JSON array format
        let json_array: serde_json::Value = serde_json::from_str(json_content)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        
        let array = json_array.as_array()
            .ok_or_else(|| "Expected JSON array format".to_string())?;
        
        let mut parser = HoudiniJsonParser::new();
        parser.parse_array(array)
    }
    
    // Convert to GLTF format that your existing system can handle
    pub fn to_gltf_json(geometry: &BevyGeometry) -> Result<String, String> {
        // Create a minimal GLTF JSON structure
        let base64_data = HoudiniJsonParser::encode_binary_data(geometry);
        let gltf = serde_json::json!({
            "asset": {
                "version": "2.0",
                "generator": format!("Houdini JSON Converter - {}", geometry.metadata.source_software)
            },
            "scenes": [{
                "nodes": [0]
            }],
            "nodes": [{
                "mesh": 0
            }],
            "meshes": [{
                "primitives": [{
                    "attributes": {
                        "POSITION": 0,
                        "NORMAL": 1
                    },
                    "indices": 2,
                    "mode": 1 // LINE_LIST
                }]
            }],
            "accessors": [
                {
                    "bufferView": 0,
                    "componentType": 5126, // FLOAT
                    "count": geometry.vertices.len(),
                    "type": "VEC3",
                    "max": [geometry.bounds[1], geometry.bounds[3], geometry.bounds[5]],
                    "min": [geometry.bounds[0], geometry.bounds[2], geometry.bounds[4]]
                },
                {
                    "bufferView": 1,
                    "componentType": 5126, // FLOAT
                    "count": geometry.normals.len(),
                    "type": "VEC3"
                },
                {
                    "bufferView": 2,
                    "componentType": 5125, // UNSIGNED_INT
                    "count": geometry.indices.len(),
                    "type": "SCALAR"
                }
            ],
            "bufferViews": [
                {
                    "buffer": 0,
                    "byteOffset": 0,
                    "byteLength": geometry.vertices.len() * 12 // 3 floats * 4 bytes
                },
                {
                    "buffer": 0,
                    "byteOffset": geometry.vertices.len() * 12,
                    "byteLength": geometry.normals.len() * 12
                },
                {
                    "buffer": 0,
                    "byteOffset": (geometry.vertices.len() + geometry.normals.len()) * 12,
                    "byteLength": geometry.indices.len() * 4
                }
            ],
            "buffers": [{
                "byteLength": (geometry.vertices.len() + geometry.normals.len()) * 12 + geometry.indices.len() * 4,
                "uri": format!("data:application/octet-stream;base64,{}", base64_data)
            }]
        });
        
        serde_json::to_string(&gltf).map_err(|e| format!("GLTF serialization error: {}", e))
    }
    
    fn encode_binary_data(geometry: &BevyGeometry) -> String {
        let mut buffer = Vec::new();
        
        // Add vertex positions
        for vertex in &geometry.vertices {
            buffer.extend_from_slice(&vertex[0].to_le_bytes());
            buffer.extend_from_slice(&vertex[1].to_le_bytes());
            buffer.extend_from_slice(&vertex[2].to_le_bytes());
        }
        
        // Add normals
        for normal in &geometry.normals {
            buffer.extend_from_slice(&normal[0].to_le_bytes());
            buffer.extend_from_slice(&normal[1].to_le_bytes());
            buffer.extend_from_slice(&normal[2].to_le_bytes());
        }
        
        // Add indices
        for &index in &geometry.indices {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        
        base64::engine::general_purpose::STANDARD.encode(&buffer)
    }
    
    fn new() -> Self {
        Self
    }
    
    fn parse_array(&mut self, array: &[serde_json::Value]) -> Result<BevyGeometry, String> {
        let mut info: Option<HoudiniGeometryInfo> = None;
        let mut vertices: Vec<[f32; 3]> = Vec::new();
        let mut primitives: Vec<BevyPrimitive> = Vec::new();
        let mut metadata = GeometryMetadata {
            source_software: "Unknown".to_string(),
            artist: "Unknown".to_string(),
            feature_id: None,
            input_name: None,
            creation_date: "Unknown".to_string(),
        };
        
        let mut i = 0;
        while i < array.len() {
            if let Some(key) = array[i].as_str() {
                match key {
                    "info" => {
                        if i + 1 < array.len() {
                            info = Some(serde_json::from_value(array[i + 1].clone())
                                .map_err(|e| format!("Error parsing info: {}", e))?);
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    "attributes" => {
                        if i + 1 < array.len() {
                            vertices = self.extract_vertices_simple(&array[i + 1])?;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    "primitives" => {
                        if i + 1 < array.len() {
                            primitives = self.parse_primitives(&array[i + 1], vertices.len())?;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            } else {
                i += 1;
            }
        }
        
        // Extract metadata from info
        if let Some(info) = &info {
            metadata.source_software = info.software.clone();
            metadata.artist = info.artist.clone();
            metadata.creation_date = info.date.clone();
        }
        
        // Generate normals (simple approach for polylines/curves)
        let normals = self.generate_normals(&vertices);
        
        // Generate indices for rendering
        let indices = self.generate_indices(&vertices);
        
        let bounds = info.as_ref()
            .map(|i| [i.bounds[0], i.bounds[1], i.bounds[2], i.bounds[3], i.bounds[4], i.bounds[5]])
            .unwrap_or([0.0; 6]);
        
        Ok(BevyGeometry {
            vertices,
            indices,
            normals,
            primitives,
            bounds,
            metadata,
        })
    }
    
    // Simplified approach - just look for coordinate arrays anywhere in the structure
    fn extract_vertices_simple(&self, value: &serde_json::Value) -> Result<Vec<[f32; 3]>, String> {
        fn find_coordinates(val: &serde_json::Value, coords: &mut Vec<[f32; 3]>) {
            match val {
                serde_json::Value::Array(arr) => {
                    // Check if this is a coordinate array [x, y, z]
                    if arr.len() == 3 {
                        if let (Some(x), Some(y), Some(z)) = (
                            arr[0].as_f64(),
                            arr[1].as_f64(), 
                            arr[2].as_f64()
                        ) {
                            coords.push([x as f32, y as f32, z as f32]);
                            return;
                        }
                    }
                    // Otherwise, recursively search
                    for item in arr {
                        find_coordinates(item, coords);
                    }
                }
                serde_json::Value::Object(obj) => {
                    for (_, v) in obj {
                        find_coordinates(v, coords);
                    }
                }
                _ => {}
            }
        }
        
        let mut coordinates = Vec::new();
        find_coordinates(value, &mut coordinates);
        
        if coordinates.is_empty() {
            Err("No coordinate data found".to_string())
        } else {
            println!("Found {} coordinate points", coordinates.len());
            Ok(coordinates)
        }
    }
    
    fn parse_primitives(&self, _prims_value: &serde_json::Value, vertex_count: usize) -> Result<Vec<BevyPrimitive>, String> {
        // Simple approach - just create one line primitive connecting all vertices
        Ok(vec![BevyPrimitive {
            primitive_type: "LineList".to_string(),
            start_index: 0,
            count: vertex_count,
            material_id: None,
        }])
    }
    
    fn generate_normals(&self, vertices: &[[f32; 3]]) -> Vec<[f32; 3]> {
        // For polylines/curves, generate simple up-facing normals
        vertices.iter().map(|_| [0.0, 0.0, 1.0]).collect()
    }
    
    fn generate_indices(&self, vertices: &[[f32; 3]]) -> Vec<u32> {
        let mut indices = Vec::new();
        
        // Connect consecutive vertices as lines
        for i in 0..(vertices.len() - 1) {
            indices.push(i as u32);
            indices.push((i + 1) as u32);
        }
        
        indices
    }
}