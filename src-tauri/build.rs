fn main() {
    // Register cfg flags set by cargo-llvm-cov so rustc doesn't warn about unknown cfgs.
    println!("cargo::rustc-check-cfg=cfg(coverage)");
    println!("cargo::rustc-check-cfg=cfg(coverage_nightly)");

    compile_rag_engine_proto();

    tauri_build::build()
}

fn compile_rag_engine_proto() {
    let manifest_dir =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let proto = manifest_dir
        .parent()
        .expect("src-tauri has repository parent")
        .join("external")
        .join("rag-engine")
        .join("engine")
        .join("proto")
        .join("engine.proto");
    let include_dir = proto
        .parent()
        .expect("engine.proto has parent directory")
        .to_path_buf();

    println!("cargo::rerun-if-changed={}", proto.display());

    if !proto.exists() {
        panic!(
            "rag-engine proto not found at {}. Run `git submodule update --init --recursive`.",
            proto.display()
        );
    }

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let staged_proto_dir = out_dir.join("rag-engine-proto");
    std::fs::create_dir_all(&staged_proto_dir).expect("create staged proto dir");
    let staged_proto = staged_proto_dir.join("engine.proto");
    let proto_source = std::fs::read_to_string(&proto).expect("read rag-engine proto");
    std::fs::write(&staged_proto, remove_service_block(&proto_source, "MCP"))
        .expect("write staged rag-engine client proto");

    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc available");
    std::env::set_var("PROTOC", protoc);

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[staged_proto], &[staged_proto_dir, include_dir])
        .expect("compile rag-engine protobuf contract");
}

fn remove_service_block(proto: &str, service_name: &str) -> String {
    let needle = format!("service {service_name} {{");
    let Some(start) = proto.find(&needle) else {
        return proto.to_string();
    };

    let mut depth = 0usize;
    let mut end = start;
    for (offset, ch) in proto[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = start + offset + ch.len_utf8();
                    break;
                }
            }
            _ => {}
        }
    }

    let mut output = String::with_capacity(proto.len());
    output.push_str(&proto[..start]);
    let remainder = &proto[end..];
    if let Some(stripped) = remainder.strip_prefix("\r\n") {
        output.push_str(stripped);
    } else if let Some(stripped) = remainder.strip_prefix('\n') {
        output.push_str(stripped);
    } else {
        output.push_str(remainder);
    }
    output
}
