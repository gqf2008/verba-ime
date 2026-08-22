//! 用 vendored protoc 生成 Prost 代码，避免构建机依赖系统 protoc。

fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc 可用");
    std::env::set_var("PROTOC", protoc);
    prost_build::Config::new()
        .compile_protos(&["proto/verba.proto"], &["proto"])
        .expect("prost 编译 verba.proto");
    println!("cargo:rerun-if-changed=proto/verba.proto");
}
