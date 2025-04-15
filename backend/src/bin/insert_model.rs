use rusqlite::{params, Connection, Result};
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    let conn = Connection::open("models.db")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            model_data BLOB NOT NULL
        )",
        params![],
    )?;

    let models = vec![
        (1, "/Users/saadmoazzam/MyStuff/Bevy/my_projects/dragndrop/frontend/assets/models/tree.gltf"),
        (2, "/Users/saadmoazzam/MyStuff/Bevy/my_projects/dragndrop/frontend/assets/models/bridge.gltf"),
        (3, "/Users/saadmoazzam/MyStuff/Bevy/my_projects/dragndrop/frontend/assets/models/helix_bridge/helix_bridge.gltf"),
        (4, "/Users/saadmoazzam/MyStuff/Bevy/my_projects/dragndrop/frontend/assets/models/blue_eyeball_free/blue_eyeball_free.gltf"),
        (5, "/Users/saadmoazzam/MyStuff/Bevy/my_projects/dragndrop/frontend/assets/models/stop_sign/stop_sign.gltf"),
    ];

    for (id, path) in models {
        if !Path::new(&path).exists() {
            eprintln!("Warning: Model file '{}' does not exist, skipping", path);
            continue;
        }
        match fs::read(&path) {
            Ok(model_data) => {
                println!("Inserting model ID={} from path '{}'", id, path);
                conn.execute(
                    "INSERT OR REPLACE INTO models (id, model_data) VALUES (?1, ?2)",
                    params![id, model_data],
                )?;
            }
            Err(e) => {
                eprintln!("Error reading '{}': {}, skipping", path, e);
            }
        }
    }

    println!("Database population complete. Check models.db for inserted models.");
    Ok(())
}