# WSLController

一个轻量的 Windows 托盘小工具，用来查看、启动并后台保活本机 WSL 发行版。

## 功能

- 自动读取 `wsl.exe -l -v` 并列出本机所有 WSL 发行版。
- 每个发行版块展示名称、运行状态、WSL 版本、默认发行版标记和保活状态。
- 每个发行版块支持打开 Shell、启动后台保活、关闭该发行版。
- 点击窗口关闭按钮时隐藏到系统托盘，不退出程序。
- 托盘右键菜单支持显示窗口、关闭全部 WSL、退出程序。
- `退出程序` 只退出管理器，并清理本工具创建的保活进程。
- `关闭全部 WSL` 才会执行 `wsl.exe --shutdown`。

## 构建

```powershell
cargo build --release
```

生成文件：

```text
target\release\wsl_controller.exe
```

也可以运行：

```powershell
.\build-release.ps1
```

## 使用

1. 启动 `target\release\wsl_controller.exe`。
2. 在列表里选择对应发行版，点击 `Shell` 打开可见终端窗口。
3. 点击 `保活` 会启动该发行版的隐藏后台保活进程。
4. 点击窗口右上角关闭时，程序会进入系统托盘。
5. 在系统托盘右键选择 `退出程序` 只退出管理器。
6. 在系统托盘右键选择 `关闭全部 WSL` 可停止全部 WSL。

## 许可证

MIT
