# canvas-todo-sync

把 Canvas LMS 的**作业、测验、公告、讨论、全局通知**同步到 **Microsoft To Do**。

这是 [1357310795/TodoSync](https://github.com/1357310795/TodoSync)（.NET 6）中
`TodoSynchronizer.CLI` + `TodoSynchronizer.Core` 的 Rust 移植，面向 Linux 服务器上的无界面运行：

- 单个可执行文件（约 3 MB），无需 .NET 运行时
- TOML 配置，所有选项都有默认值，写错字段名会直接报错
- 使用 systemd timer 定时运行，日志进入 journald
- 与原版生成的待办事项兼容：原版创建的任务会被识别，不会重复创建

## 功能

| Canvas 条目 | 待办事项内容 |
| --- | --- |
| 作业 | 标题、描述（备注）、截止日期、提醒、提交状态/评分/老师评论（步骤）、描述中的文件与图片（附件） |
| 测验（经典测验） | 标题、描述、截止日期、提醒、每次尝试的分数（步骤）、附件 |
| 公告 / 讨论 | 标题、发布者与内容、发布日期、提醒、附件 |
| 全局通知 | 标题、内容、开始/结束日期、提醒、重要性 |

- **自动管理与去重**：每个待办事项带有指向 Canvas 页面的“关联链接”，以此识别条目。
  已存在则按 `update_*` 规则更新，不存在则按 `create_*` 规则创建；
  可选择自动删除重复的任务（`sync.remove_duplicates`）。
- **不覆盖你的修改**：把对应的 `update_*` 设为 false，你在 To Do 中改过的标题、备注、日期等就不会被覆盖。
- **dry-run**：`sync --dry-run` 只显示将要做的修改，不写入 To Do。

### 相比原版的修正与改动

- 测验的 `title_template`、`due_date_mode`、`remind_mode`、`remind_before` 现在会真正生效
  （原版中测验被当作作业处理，实际使用的是作业的设置）
- Canvas 分页不再受“最多 9 页”的限制；被禁用的公告/讨论栏目会被安全跳过
- 单个条目出错不再中断整门课程的同步；To Do 中有重复任务时不再崩溃
- Canvas 地址、时区可配置（原版写死了 `oc.sjtu.edu.cn` 和 UTC+8）
- 新建任务时一次性写入关联链接，避免中途失败产生无法识别的“孤儿”任务
- 截止日期按配置的时区取日期（原版按 UTC 取日期，北京时间 0–8 点截止的作业会显示为前一天）
- 只向 Canvas 自身域名发送 Canvas 令牌；遇到限流（HTTP 429）或临时故障自动重试
- 移除了 GitHub Actions 相关的令牌加密逻辑（本地运行不需要），暂不支持滴答清单

## 安装

```sh
cargo build --release
install -Dm755 target/release/canvas-todo-sync ~/.local/bin/canvas-todo-sync
```

## 配置

### 1. 生成配置文件

```sh
canvas-todo-sync init        # 生成 ~/.config/canvas-todo-sync/config.toml
```

文件中的每个选项都有注释，说明见 [config.example.toml](config.example.toml)。
按需修改 `canvas.base_url`（默认为上海交通大学 Canvas）。

### 2. Canvas 令牌

在 Canvas 中进入“设置 → 创建新访问许可证”生成令牌，然后任选一种方式提供：

```sh
# 推荐：单独的文件
echo '你的令牌' > ~/.config/canvas-todo-sync/canvas_token
chmod 600 ~/.config/canvas-todo-sync/canvas_token
# 并在 config.toml 的 [canvas] 中设置 token_file = "canvas_token"
```

也可以直接写在 `config.toml` 的 `canvas.token` 中，或通过环境变量 `CANVAS_TOKEN` 提供。

### 3. Microsoft 账户

```sh
canvas-todo-sync auth login
```

按提示在任意设备的浏览器中打开链接、登录授权，然后把跳转后的空白页地址粘贴回终端。
详细说明（包括使用自己注册的应用、设备代码登录）见 [docs/microsoft-auth.md](docs/microsoft-auth.md)。

### 4. 检查并试运行

```sh
canvas-todo-sync check            # 检查配置与两侧凭据
canvas-todo-sync sync --dry-run   # 预览将要进行的修改
canvas-todo-sync sync             # 正式同步
```

如果是学期中途开始使用，建议先在配置中设置 `ignore_too_old_items = true`，跳过两周前就已过期的条目。

## 用 systemd 定时运行

使用 systemd **用户服务**，不需要 root 权限：

```sh
mkdir -p ~/.config/systemd/user
cp contrib/systemd/canvas-todo-sync.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now canvas-todo-sync.timer

# 让用户服务在未登录 SSH 时也能运行（只需执行一次）
sudo loginctl enable-linger "$USER"
```

常用命令：

```sh
systemctl --user list-timers canvas-todo-sync.timer   # 查看下次运行时间
systemctl --user start canvas-todo-sync.service       # 立即运行一次
systemctl --user status canvas-todo-sync.service      # 查看上次运行结果
journalctl --user -u canvas-todo-sync -e              # 查看日志
journalctl --user -u canvas-todo-sync -p warning      # 只看警告和错误
```

- 修改运行频率：`systemctl --user edit canvas-todo-sync.timer`，覆盖 `OnCalendar=`
  （例如 `*:0/30` 每半小时、`*-*-* 07..23:00` 每天 7 点到 23 点每小时），
  然后 `systemctl --user daemon-reload`。
- 同步出错时程序以非零状态退出，服务会显示为 failed。可以配合 `OnFailure=` 发送通知；
  如果有可以忽略的错误反复出现，可设置 `sync.ignore_errors = true`。
- 如果二进制不在 `~/.local/bin`，修改 service 文件中的 `ExecStart=`。

## 文件位置

| 用途 | 默认路径 | 覆盖方式 |
| --- | --- | --- |
| 配置文件 | `~/.config/canvas-todo-sync/config.toml` | `-c` / `CANVAS_TODO_SYNC_CONFIG` / systemd `ConfigurationDirectory=` |
| Microsoft 令牌 | `~/.local/state/canvas-todo-sync/ms_token.json` | `--state-dir` / `microsoft.token_file` / systemd `StateDirectory=` |

## 代码结构

```
src/
├── main.rs              命令行入口（sync / auth / init / check）
├── config.rs            TOML 配置与默认值
├── http.rs              HTTP 客户端、重试与错误处理
├── canvas/              Canvas REST API 客户端与数据模型
├── todo/
│   ├── mod.rs           TodoBackend trait：待办服务的抽象接口
│   └── microsoft/       Microsoft To Do 实现（Graph API）与 OAuth 认证
├── sync/
│   ├── mod.rs           通用同步引擎：去重、字段比较、步骤与附件同步
│   └── canvas.rs        Canvas 来源：把课程条目转换为 SyncItem
└── text/                HTML 转文本、模板、附件提取、时长解析
```

扩展方式：

- **接入其他待办服务**（如滴答清单）：在 `src/todo/` 下实现 `TodoBackend` trait，同步引擎无需改动。
- **接入其他数据来源**：参照 `sync/canvas.rs`，把条目转换为 `SyncItem` 后交给 `Reconciler::sync_item`。

运行测试：`cargo test`。

## 许可证

GPL-3.0，与原项目相同。
