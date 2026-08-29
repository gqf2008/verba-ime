fn main() {
    slint_build::compile("ui/settings.slint").unwrap();
    // 嵌入应用图标（资源管理器/任务栏显示）。
    embed_resource::compile("../../assets/branding/verba.rc", embed_resource::NONE);
}
