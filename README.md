<div align="center">

# CarbonPaper · 复写纸

**找回你在电脑上看过的内容。**

[![最新版本](https://img.shields.io/github/v/release/White-NX/carbonPaper)](https://github.com/White-NX/carbonPaper/releases/latest)
![Windows x64](https://img.shields.io/badge/platform-Windows%20x64-0078D4)
[![许可证：GPLv3](https://img.shields.io/badge/license-GPLv3-blue)](./LICENSE)
[![发布构建](https://github.com/White-NX/carbonPaper/actions/workflows/release.yml/badge.svg)](https://github.com/White-NX/carbonPaper/actions/workflows/release.yml)

**简体中文** · [English](./README.en.md)

[下载](https://github.com/White-NX/carbonPaper/releases/latest) · [快速上手](#快速上手) · [开发与贡献](#开发与贡献) · [报告问题](https://github.com/White-NX/carbonPaper/issues)

</div>

CarbonPaper 是一款面向 **Windows** 的开源屏幕历史工具。它按你的设置记录屏幕快照，支持通过文字、画面描述和时间线找回历史内容，并在本机加密保存截图与识别出的文字。

![CarbonPaper 界面预览](./docs/imgs/carbonpaper-screenshots.png)

## 功能亮点

- **文字与画面搜索**：输入记得的关键词，或用一句话描述画面，查找相关快照。
- **时间线回看**：按时间浏览历史记录，查看当时的画面和识别出的文字。
- **智能归档**：写下你关心的主题，自动归档符合描述的快照；首次启用需要准备相应组件。
- **每日回顾**：连接你选择的 AI 模型服务，概括一天的活动和进展，并回到来源记录查看细节。
- **记录范围由你控制**：随时暂停记录，按应用或窗口标题设置过滤规则，按时间范围清除历史。
- **浏览器与 AI 工具集成**：通过 Chrome / Edge 扩展保存网页来源，通过 MCP 向授权的 AI 助手提供历史记录。

## 下载与安装

从 [Releases](https://github.com/White-NX/carbonPaper/releases/latest) 下载最新版本，发行说明中列有该版本的功能和变更。

| 下载文件 | 使用方式 |
| --- | --- |
| `*-setup.exe` 安装包 | 推荐日常使用，运行后按安装向导完成安装。 |
| `*_x64_portable.zip` 便携包 | 完整解压到一个文件夹，保留随包提供的文件，运行 `carbonpaper.exe`。 |

### 运行环境

| 项目 | 说明 |
| --- | --- |
| 平台 | Windows 桌面，x64；推荐使用已更新的 Windows 11。 |
| 身份验证 | 已配置且可用的 Windows Hello，例如 PIN、指纹或人脸识别。 |
| 桌面运行时 | [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)；安装包会检查并按需安装。 |
| 推理 | 支持 CPU 推理，可使用兼容 DirectML 的 GPU 加速；无需专用 NPU。 |
| 网络 | 下载组件、检查更新和调用云端 AI 服务时需要联网。本地功能的离线使用见[常见问题](#常见问题)。 |
| 存储 | 为程序组件、截图和索引预留空间，可在应用内设置历史保留时长和存储上限。 |

首次启动会检查所需组件，并提示完成下载。额外组件的下载需求会随所选功能而变化。

启用“重启后继续后台整理”时，应用会请求管理员授权安装本机服务，并自动重启一次。安装版和便携版使用相同的启用流程。

## 快速上手

1. **完成首次设置**：启动 CarbonPaper，按提示通过 Windows Hello 验证，选择推荐设置或自定义功能、资源占用和启动方式。
2. **准备所需组件**：等待基础组件就绪。启用智能功能时，向导会显示额外下载及其进度。
3. **设置记录范围**：在“设置 → 记录与采集”中添加应用和窗口标题过滤规则。通过顶栏按钮开始或暂停记录；若选择了自动记录，基础组件就绪后会自动开始。
4. **找回历史内容**：积累一些记录后，在“高级搜索”中选择“文字”输入关键词，或选择“画面”描述内容；也可以在“预览”中沿时间线浏览快照。

例如，文字搜索可以查找你看过的项目名称；画面搜索可以尝试“海洋”这样的描述。搜索范围是已经保存的历史记录。

## 隐私与数据安全

截图与识别出的文字在本机加密保存，访问记录通过 Windows Hello 验证。解锁有效期和后台处理权限可在“设置 → 隐私与访问”中调整。

不同功能的数据处理方式如下：

| 功能 | 数据处理方式 |
| --- | --- |
| 屏幕记录、OCR、文字与画面检索、智能归档 | 在本机处理历史记录。 |
| 组件下载与程序更新 | 连接下载源，获取所需文件。 |
| AI 搜索与每日回顾 | 将任务所需的记录内容发送给你选择的模型服务；服务可以运行在本机或云端。 |
| 外部 AI 助手 / MCP | 向通过认证的客户端提供记录；客户端如何继续处理数据取决于其配置。 |

MCP 服务仅监听本机地址，使用访问令牌认证，数据查询还要求应用处于解锁状态。“AI 助手访问”提供敏感内容过滤和个人信息脱敏设置。使用云端模型或外部助手时，请根据自己的数据使用需求配置这些功能。

## 进阶使用

### 浏览器扩展

Chrome 和 Edge 扩展可为浏览器记录保存页面网址、标题和链接，方便重新打开原网页。

1. 在“设置 → 记录与采集 → 浏览器扩展”中选择对应浏览器，点击“设置浏览器”。
2. 在打开的扩展管理页面启用“开发者模式”，选择“加载已解压的扩展程序”，使用 CarbonPaper 显示的扩展文件夹。
3. 保持 CarbonPaper 运行，确认对应浏览器显示“已连接”，并开启“启用扩展增强”。

### AI 搜索与每日回顾

在“设置 → AI → 模型服务”中添加服务并测试连接。可以使用云端服务，也可以连接本机运行的 Ollama 或 LM Studio。

配置完成后，在“高级搜索”的“AI”模式中提问，让模型查找记录并回答。每日回顾在“设置 → 整理与智能 → 每日回顾”中单独启用，并可选择用于生成回顾的模型。

这些功能使用你配置的模型服务；文字搜索、画面搜索和智能归档使用本地组件。

### MCP 接入

CarbonPaper 内置 MCP 服务，支持使用 Streamable HTTP 的客户端查询截图历史、识别文字、智能归档和每日回顾。

1. 保持应用运行并解锁，在“设置 → AI → AI 助手访问”中启用 MCP 服务。
2. 选择目标 Agent，复制应用提供的配置提示词，使用页面显示的端点和访问令牌完成连接。
3. 运行“MCP 连接自检”，确认服务状态和认证结果。

将令牌保存在客户端的私密配置中，避免写入聊天、日志或仓库。工具定义见 [MCP 工具契约](./docs/mcp-tool-contract-v2.json)，每日回顾的接口说明见[回顾查询工具文档](./docs/recap-query-tools.md)。

## 常见问题

<details>
<summary>可以离线使用吗？</summary>

所需组件准备完成后，屏幕记录、OCR、文字与画面检索、智能归档可以在本机离线运行。下载组件、检查更新和调用云端模型需要联网。AI 搜索与每日回顾也可以连接本机模型服务。

</details>

<details>
<summary>为什么启动后没有看到新记录？</summary>

先检查顶栏的记录状态和组件下载进度。未启用自动记录时，需要手动开始。资源占用策略可能在电池供电、游戏或全屏应用运行时暂停记录；应用和窗口标题过滤规则也会影响采集范围。可以在“设置 → 通用”和“设置 → 记录与采集”中查看对应设置。

</details>

<details>
<summary>会占用多少空间，如何管理历史？</summary>

空间占用取决于启用的组件、屏幕分辨率、记录时长和历史保留设置。在“设置 → 存储与维护”中可以查看实际用量、修改存储位置、设置保留时长和存储上限，也可以清除指定时间范围的记录。下载提示会显示相关组件的大小。

</details>

<details>
<summary>如何备份或迁移到另一台电脑？</summary>

在“设置 → 存储与维护”中导出带密码保护的备份，再通过目标安装中的备份导入功能恢复。导出需要先解锁记录，恢复时需要备份密码。请保管好备份文件及其密码。

</details>

## 开发与贡献

项目使用 React、Vite 和 Tailwind CSS 构建界面，Tauri v2 / Rust 实现桌面功能与后台处理，SQLite / SQLCipher 管理本地存储。

在 Windows 上准备 Node.js **22.18.0 或更新版本**、Rust stable，以及 [Tauri Windows 开发依赖](https://v2.tauri.app/start/prerequisites/#windows)（Microsoft C++ Build Tools 与 WebView2），然后运行：

```powershell
git clone https://github.com/White-NX/carbonPaper.git
cd carbonPaper
npm install
npm run debug
```

`npm run debug` 会准备开发资源并启动完整桌面应用，使用当前用户已有的数据和设置，并按需请求管理员授权。开发服务的工作方式见[真实 Windows 服务调试说明](./docs/app-bound-processing.md#debug-with-the-real-windows-service)。

| 命令 | 用途 |
| --- | --- |
| `npm run dev` | 单独启动 Vite 前端开发服务器。 |
| `npm run build` | 执行 TypeScript 检查并构建前端。 |
| `npm run lint:ci` | 检查前端与浏览器扩展的 ESLint 错误。 |
| `npm run test:frontend` | 运行前端测试。 |
| `npm run test:backend:fast` | 运行安全检查和 Rust 库测试。 |
| `npm run i18n:check` | 检查翻译键和语言一致性。 |

完整发布打包使用 `npm run tauri:build`，需要配置发布签名；具体要求见[构建与自动化检查](./docs/app-bound-processing.md#build-and-automated-checks)。

欢迎提交问题、功能建议、翻译改进和 Pull Request。开发约定见[贡献指南](./CONTRIBUTING.md)，界面开发参考 [UI 覆盖层约定](./docs/ui-overlay-conventions.md)。

- 错误反馈与功能建议：[GitHub Issues](https://github.com/White-NX/carbonPaper/issues)。
- 安全漏洞报告：请遵循[安全政策](./SECURITY.md)中的报告方式。

## 许可证与致谢

CarbonPaper 使用 [GNU GPL v3](./LICENSE) 许可证。第三方组件和模型遵循各自的许可证。

感谢以下开源项目：

- [Tauri](https://github.com/tauri-apps/tauri) 与 [React](https://github.com/facebook/react)：桌面框架和用户界面。
- [RapidOCR](https://github.com/RapidAI/RapidOCR)：文字识别。
- [ONNX Runtime](https://github.com/microsoft/onnxruntime) 与 [DirectML](https://github.com/microsoft/DirectML)：本地推理。
- [Chinese-CLIP](https://github.com/OFA-Sys/Chinese-CLIP)、[Sentence Transformers](https://github.com/UKPLab/sentence-transformers) 与 [BGE](https://github.com/FlagOpen/FlagEmbedding)：画面和文本检索、语义匹配。
- [SQLite](https://www.sqlite.org/) 与 [SQLCipher](https://github.com/sqlcipher/sqlcipher)：本地数据存储。

感谢以下社区：

- [linux.do](https://linux.do)

“CarbonPaper”取自复写纸：在书写时留下副本。我们希望它也能帮助你留下可检索的屏幕记忆。
