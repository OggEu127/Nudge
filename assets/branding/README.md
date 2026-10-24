# Nudge 品牌资源

本目录的来源是项目所有者提供的 `nudge-logo-blue-violet-assets.zip`（2026-10-24，40,985 字节，SHA-256 `8ced336d5b7d1dd52a4b834ddf23dc27516d7c903f74bc6989b925e3dc64d7da`）。源归档不进入版本库；资源随仓库以 GPL-3.0-only 发布。

| 文件 | 用途 |
| --- | --- |
| `nudge-icon-blue-violet.ico` | Windows 可执行文件图标（16、24、32、48、64、128、256） |
| `nudge-icon-blue-violet.png` | 运行时窗口/任务栏图标源图（512×512，RGBA；加载时按预乘 alpha 缩放到 128×128） |
| `nudge-icon-blue-violet.svg` | 图标矢量源文件 |
| `nudge-logo-blue-violet.png` | 应用内标题栏图集：图标与字标通过 UV 裁切使用（1508×488，RGBA） |
| `nudge-logo-blue-violet.svg` | 字标矢量源文件 |
| `nudge-lockup-blue-violet.png` | README 标题图（840×269，浅色主题；由主图裁切并移除过小的点阵副标题） |
| `nudge-lockup-blue-violet-dark.png` | README 标题图深色主题变体（把字标的蓝色与紫色向白色提亮，图标保持原色） |
| `nudge-preview-blue-violet.png` | 品牌预览图，供后续宣传材料使用 |

`nudge-lockup-*` 是主图的派生文件，只用于 README；界面运行时仍解码内置主图，不读取这些文件。两个派生图使用预乘 alpha 的 Lanczos 缩放生成，避免透明像素在边缘产生暗边。替换主图后必须同步重新生成，并更新下方摘要。

## 来源校验

解压后的文件与源归档逐字节一致，SHA-256：

```text
ee6c78a4c732f4e16969d360aaad8c1b8db967c7a8bf621cb5a4c2a4db7c97dc  nudge-icon-blue-violet.ico
1cf947e2555bf46d837a6daafa1b1ca3b8ae523e3633f2f944f1fecb6b43edf5  nudge-icon-blue-violet.png
11a3b06b612e2cb4be45b375b8a5888d5ac7112c1aa233a9b00e9268738b3b7f  nudge-icon-blue-violet.svg
c4a435bde45672eb5bd5dd4acf0c8e46246aa92f7b68bf24e74ab3fa539dedd3  nudge-logo-blue-violet.png
3df5ee37e1b982ebf88b31382691b65ee56e3468c84f5f12bab73f09df928e56  nudge-logo-blue-violet.svg
3e827075fd3f30635cc2a632b61c873f527436dd8949a8755c8edaa841489ca8  nudge-preview-blue-violet.png
```

派生图摘要：

```text
29d68596fd8d0309b0a53d957bd7fbdb81f1a56432473b004aa86034ac7df4e8  nudge-lockup-blue-violet.png
893242e490e7c55d17209f9eb8e9843e4b2724d282758b5fd8cf6736f74b77ff  nudge-lockup-blue-violet-dark.png
```

两个 SVG 只包含 `svg`、`title`、`polygon`、`rect`、`g`、`text` 元素和表现属性，不含脚本、`foreignObject`、外部引用或事件处理器；其中字标使用本机 Arial/Helvetica 字体名，不嵌入字体文件。界面运行时只解码内置 PNG；构建脚本只把 ICO 交给 Windows 资源编译器，不执行归档中的任何内容。

`crates/launcher-gui/src/branding.rs` 的测试会解析 ICO 目录，检查 7 个尺寸、32 位深度和每帧 PNG 签名，并验证窗口图标解码；这不能替代在 Windows 上查看 Explorer/任务栏的实际渲染。
