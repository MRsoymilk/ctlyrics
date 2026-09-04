# ctlyrics

cmus terminal lyrics, 终端显示 `cmus` 播放器的歌词

![ctlyrics](README/ctlyrics.jpg)

## 项目结构

- `src/main.rs` - 入口点，CLI，TUI 事件循环
- `src/player.rs` - TUI 渲染/输入处理
- `src/lyrics_cache.rs` - 从 `lyrics/` 目录加载/解析 `.lrc` 文件
- `src/logger.rs` - Tracing 日志，支持每日轮转
- `src/cmus.rs` - `cmus-remote -Q` 封装
- `src/downloader.rs` - 从 sq0527.cn 下载歌词
- `src/song_list.rs` - 从音乐目录生成 `songs_list.txt`

## 原理

使用 `cmus-remote -Q` 查询当前音乐状态，查找对应的歌词进行时间轴匹配

## 使用

### 显示歌词（主功能）

```bash
cargo run
```

### 生成 songs_list.txt

```bash
cargo run -- generate /path/to/music
```

### 下载歌词

```bash
cargo run -- download
```

### 控制键

| 键 | 功能 |
|-----|--------|
| `q` | 退出 |
| `←`/`→` | 偏移调整 ±0.1s |
| `↑`/`↓` | 偏移调整 ±0.5s |
| `:` | 命令模式（输入 `love` 可触发彩蛋） |

## 依赖

- Rust 1.70+
- `cmus` + `cmus-remote` 必须已安装并运行

## 目录结构

```
lyrics/           # .lrc 文件 (gitignored)
songs_list.txt    # 生成的歌曲列表 (gitignored)
ctlyrics.log*     # 日志输出 (gitignored)
```

## 歌词文件格式

预期 `.lrc` 格式：`[mm:ss.xx]lyric text`
由 `LyricsCache.parse_lrc()` 解析为 `(timestamp_seconds, text)` 元组列表

## 歌曲匹配逻辑

`LyricsCache.find_lrc_file()` 按以下顺序匹配：
1. 标题（不区分大小写正则）在文件名中
2. 多个匹配时，艺术家名作为决胜因素
3. 回退到第一个匹配

## 注意事项

- 需要活跃的 `cmus` 会话（`cmus-remote -Q` 必须工作）
- `song_list.rs` 默认路径可通过 CLI 参数修改
- 从 sq0527.cn 下载歌词可能不可靠（网站曾宕机过）
- 暂无测试套件
- 暂无 lint/typecheck 配置（可添加 `cargo clippy` 和 `rustfmt`）