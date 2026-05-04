const isMacOS = process.platform === "darwin";

const command = isMacOS
  ? ["bun", "run", "tauri", "build", "--bundles", "app"]
  : ["cargo", "build", "--manifest-path", "src-tauri/Cargo.toml"];

const label = isMacOS
  ? "Building macOS Tauri app bundle"
  : "Building Tauri Rust backend";

console.log(label);

const result = Bun.spawnSync(command, {
  stdout: "inherit",
  stderr: "inherit",
});

process.exit(result.exitCode ?? 1);
