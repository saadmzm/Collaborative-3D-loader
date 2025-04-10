use rusqlite::{params, Connection, Result};

fn main() -> Result<()> {
    let conn = Connection::open("models.db")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL
        )",
        params![],
    )?;
    let models = vec![
        (1, "models/tree.gltf"),
        (2, "models/bridge.gltf"),
        (3, "models/helix_bridge/scene.gltf"),
        (4, "models/blue_eyeball_free/scene.gltf"),
        (5, "models/stop_sign/scene.gltf"),
    ];
    for (id, path) in models {
        conn.execute("INSERT OR REPLACE INTO models (id, path) VALUES (?1, ?2)", params![id, path])?;
    }
    Ok(())
}