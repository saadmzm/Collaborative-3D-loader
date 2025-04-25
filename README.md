# Collaborative 3D Model Loader

## Setup

- Install [Rust](https://www.rust-lang.org/tools/install).
- Clone this repository.
- Change directory to this repository in the terminal.

## Usage

- Run the Backend and Frontend in different terminals.

### Backend

- Start the Server.
  
```bash
cargo run --release
```

- To close the server press `Ctrl+C`.

### Native Frontend

- Start the Client.

```bash
cargo run --release --bin frontend
```

### Web Browser Frontend

- Start the web client.

```bash
python3 -m http.server 3000
```

- Go to `localhost:3000` in your web browser.

## Additional Notes

- You can add 3D models from the Dialog box in the native client window.
- Currently only Embedded Gltf 3D models work, more formats will be added in future.
- Web clients can only load and view the 3d models.
