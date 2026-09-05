# ctlyrics

`ctlyrics` 是一个配合 [cmus](https://cmus.github.io/) 使用的终端歌词显示程序。它通过 `cmus-remote -Q` 获取当前歌曲和播放进度，读取 LRC 文件并同步显示歌词。

![ctlyrics](README/ctlyrics.jpg)

## 功能

- 在终端中同步显示 cmus 当前歌曲歌词
- 优先读取 cmus 的 `title`、`artist` 标签，并支持从 `歌曲名-歌手.ext` 文件名回退解析
- 支持按音乐文件路径或标题、歌手匹配歌词
- 支持使用方向键调整歌词时间偏移
- 支持终端窗口缩放并自动重新绘制
- 不使用备用屏幕，退出后保留当前终端会话
- 内置歌词映射管理网页
- 网页支持拖拽上传一个或多个 `.lrc` 文件
- 网页保存音乐目录时显示加载进度
- 网页修改映射后，终端自动重新加载映射配置

## 环境要求

- Rust 1.85 或更高版本（项目使用 Rust 2024 Edition）
- 已安装并运行 `cmus`
- `cmus-remote -Q` 能够正常返回当前歌曲信息
- 使用 `:web` 时需要可用的图形浏览器

可以先检查 cmus 状态：

```bash
cmus-remote -Q
```

## 构建

```bash
cargo build
```

发布构建：

```bash
cargo build --release
```

生成的主程序位于：

```text
target/debug/ctlyrics
target/release/ctlyrics
```

## 使用

### 启动歌词界面

在项目根目录运行：

```bash
./target/debug/ctlyrics
```

也可以直接使用 Cargo：

```bash
cargo run
```

### 控制键

| 按键 | 功能 |
|---|---|
| `q` | 退出程序 |
| `←` / `→` | 歌词时间偏移 `-0.1s` / `+0.1s` |
| `↑` / `↓` | 歌词时间偏移 `-0.5s` / `+0.5s` |
| `:` | 进入命令模式 |
| `Esc` | 退出命令模式 |

### 命令模式

在歌词界面按 `:`，输入命令后按回车：

| 命令 | 功能 |
|---|---|
| `:web` | 即时启动 `http://localhost:3000` 并使用默认浏览器打开 |

Web 服务在当前 `ctlyrics` 进程内后台运行，不需要提前单独启动。

### 单独启动 Web 服务

也可以不启动 TUI，单独运行配置网页：

```bash
./target/debug/ctlyrics web
```

指定端口：

```bash
./target/debug/ctlyrics web --port 3001
```

## 网页配置歌词

1. 使用 `:web` 打开配置页面。
2. 点击“去设置”或“修改”，填写音乐目录的绝对路径。
3. 保存后等待顶部进度条结束，页面会列出扫描到的音乐文件。
4. 将 `.lrc` 文件拖入网页任意位置，或者在映射窗口中点击上传区域选择文件。
5. 点击歌曲旁的“添加映射”。
6. 选择对应的歌词文件并保存。

上传规则：

- 仅允许 `.lrc` 文件
- 支持同时上传多个文件
- 单次请求最大 10 MiB
- 上传结果保存在当前工作目录的 `lyrics/` 中
- 上传完成后，歌词文件会立即加入映射下拉列表

映射保存后，正在运行的 TUI 会检测 `config/mappings.json` 的变化并重新加载，不需要重新启动程序。

## 歌词匹配顺序

程序按以下顺序查找歌词：

1. 使用 cmus 返回的音乐文件完整路径查找映射
2. 使用标题和歌手查找映射
3. 在 `lyrics/` 中查找文件名包含歌曲标题的 `.lrc` 文件
4. 如果有多个候选文件，再使用歌手名称筛选

网页中显示“已映射”只代表该行歌曲已配置歌词，不代表 cmus 当前正在播放这首歌曲。可以使用下面的命令确认当前歌曲：

```bash
cmus-remote -Q
```

## LRC 格式

支持标准时间标签：

```text
[00:00.00]歌曲名称
[00:10.70]第一句歌词
[01:05.32]下一句歌词
```

`[ti:]`、`[ar:]`、`[al:]` 等元数据不会显示为歌词。

## 工作目录

所有运行数据都相对于启动程序时的当前工作目录，而不是可执行文件所在目录：

```text
config/mappings.json   # 音乐目录和歌词映射
lyrics/                # LRC 歌词文件
log/                   # 程序日志
```

例如：

```bash
cd target/debug
./ctlyrics
```

此时程序使用 `target/debug/config/` 和 `target/debug/lyrics/`。

如果在项目根目录运行：

```bash
./target/debug/ctlyrics
```

则程序使用项目根目录下的 `config/` 和 `lyrics/`。建议固定在同一目录启动，避免读取到不同的配置和歌词。

## 项目结构

```text
src/
  main.rs          主程序入口和 TUI 事件循环
  player.rs        TUI 渲染、输入和命令处理
  cmus.rs          cmus-remote 查询与歌曲信息解析
  lyrics_cache.rs  映射刷新、LRC 查找和解析
  mapping.rs       映射配置读写
  web.rs           Web 路由、上传和映射接口
  logger.rs        日志初始化
templates/
  index.html       Web 管理页面
```

## 开发验证

```bash
cargo build
cargo test --no-run
```
