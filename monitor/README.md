# monitor

这是一个由 Tauri 启动的本地截图守护进程的示例实现。它会每隔 5 秒截取当前焦点窗口并保存到 `screenshots/` 目录。IPC 使用 Windows 命名管道，并提供两种更安全的父进程集成方式：

- 通过一次性随机管道名（由 monitor 打印到 stdout，或由父进程通过环境变量传入）
- 通过句柄继承（父进程创建可继承的管道句柄并传入子进程）——更安全，因为不会在系统上公开可猜测的名称

快速上手
1. 安装依赖：

```powershell
pip install -r requirements.txt
```

2. 直接运行（monitor 会生成随机管道名并打印到 stdout）：

```powershell
python main.py
# 输出一行随机管道名，例如：carbon_monitor_9f7a2e...
```

3. 通过环境变量指定管道名（由父进程生成并传入）：

```powershell
$env:CARBON_MONITOR_PIPE = 'my_secure_pipe_123'
python main.py
```

4. 更安全的句柄继承（推荐，适用于由 Tauri/Rust 启动 monitor 的场景）

父进程（Rust/Tauri）应：
- 创建一个命名管道（使用 WinAPI CreateNamedPipe），并确保创建的句柄可继承（或调用 DuplicateHandle 以生成可继承句柄）。
- 将该句柄的数值（整数）通过环境变量 `CARBON_MONITOR_PIPE_HANDLE` 传给 `monitor` 可执行进程，并以继承句柄的方式启动子进程。

在 `monitor` 中，它会检测到 `CARBON_MONITOR_PIPE_HANDLE` 并把该句柄当作服务器句柄继续使用，从而避免在系统上暴露管道名。

Rust（示例）
```rust
// 伪代码，需引入 windows 或 winapi crate 并处理错误
use std::process::Command;
use std::os::windows::io::IntoRawHandle;

// 使用 windows crate 创建命名管道并确保句柄可继承（示例性描述）
// let pipe_handle = CreateNamedPipeW(..., FILE_FLAG_OVERLAPPED, ...);
// SetHandleInformation(pipe_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);

// 将句柄数值传给子进程
let handle_value = /* numeric HANDLE value */;
let mut cmd = Command::new("path\\to\\python.exe");
cmd.arg("main.py");
cmd.env("CARBON_MONITOR_PIPE_HANDLE", format!("{}", handle_value));
// 以继承句柄方式启动子进程
let child = cmd.spawn().expect("failed to spawn");
```
如何从 Tauri/其它进程与 monitor 通信

发送 JSON 请求并读取 JSON 响应。例如，使用命名管道客户端向服务端发送：

请求格式：{"command": "pause"} 或 {"command": "resume"} / {"command": "stop"} / {"command": "status"}

返回示例：{"status":"paused"} 或 {"paused": false, "stopped": false, "interval": 5}
