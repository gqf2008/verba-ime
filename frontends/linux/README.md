# Linux 前端（Fcitx5 / IBus / Wayland / XIM）

- **首选 Fcitx5 原生插件**：C++ shim + Rust 静态库（[fcitx5-afrim](https://github.com/fodydev/fcitx5-afrim) / corrosion 范式）。
- 兼容后端（按环境自动选择）：
  - **IBus**（D-Bus，imekit `ibus` feature / zbus）——GNOME 默认环境。
  - **Wayland `zwp_input_method_v2`**（imekit）——sway / Hyprland / KDE。
  - **X11 XIM**——legacy 回退。
- 打包：.deb / .rpm / AppImage。
- 状态：**未开始（M2）**。