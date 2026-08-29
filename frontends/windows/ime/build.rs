fn main() {
    // 嵌入应用图标（TSF DLL 与 verba-reg/verba-trigger 共用一个资源脚本）。
    // 本 workspace 位于 frontends/windows/ime，上三级到仓库根。
    embed_resource::compile("../../../assets/branding/verba.rc", embed_resource::NONE);
}
