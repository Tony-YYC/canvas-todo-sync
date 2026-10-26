mod canvas;
mod config;
mod http;
mod logger;
mod paths;
mod sync;
mod text;
mod todo;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use jiff::Timestamp;

use canvas::CanvasClient;
use config::Config;
use sync::Reconciler;
use sync::canvas::CanvasSync;
use todo::TodoBackend;
use todo::microsoft::{self, auth};

#[derive(Parser)]
#[command(
    version,
    about = "把 Canvas LMS 的作业、测验、公告、讨论同步到 Microsoft To Do"
)]
struct Cli {
    /// 配置文件路径 [默认: ~/.config/canvas-todo-sync/config.toml]
    #[arg(short, long, global = true, env = "CANVAS_TODO_SYNC_CONFIG")]
    config: Option<PathBuf>,

    /// 状态目录，用于保存 Microsoft 令牌 [默认: ~/.local/state/canvas-todo-sync]
    #[arg(long, global = true, env = "CANVAS_TODO_SYNC_STATE_DIR")]
    state_dir: Option<PathBuf>,

    /// 输出调试日志
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 执行一次同步
    Sync {
        /// 只读取并输出将要进行的修改，不写入 To Do
        #[arg(long)]
        dry_run: bool,
    },
    /// Microsoft 账户认证
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// 生成带注释的默认配置文件
    Init {
        /// 覆盖已存在的配置文件
        #[arg(long)]
        force: bool,
    },
    /// 检查配置文件与 Canvas、Microsoft 凭据是否可用
    Check,
}

#[derive(Subcommand)]
enum AuthAction {
    /// 登录 Microsoft 账户并保存 refresh token
    Login {
        /// 使用设备代码流（需自行注册并开启“允许公共客户端流”的应用）
        #[arg(long)]
        device: bool,
    },
    /// 显示当前登录状态
    Status,
    /// 删除保存的令牌
    Logout,
}

struct Ctx {
    config_path: PathBuf,
    state_dir: PathBuf,
}

impl Ctx {
    fn config(&self) -> Result<Config> {
        if !self.config_path.exists() {
            bail!(
                "配置文件 {} 不存在，请先运行 `canvas-todo-sync init` 生成",
                self.config_path.display()
            );
        }
        Config::load(&self.config_path)
    }

    /// 认证命令允许没有配置文件，此时使用默认值
    fn config_or_default(&self) -> Result<Config> {
        if self.config_path.exists() {
            Config::load(&self.config_path)
        } else {
            Config::parse("", Path::new("."))
        }
    }

    fn token_store(&self, cfg: &Config) -> auth::TokenStore {
        auth::TokenStore::new(cfg.ms_token_file(&self.state_dir))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    logger::init(if cli.verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    let ctx = Ctx {
        config_path: cli.config.unwrap_or_else(paths::default_config_file),
        state_dir: cli.state_dir.unwrap_or_else(paths::default_state_dir),
    };
    let res = match cli.command {
        Command::Sync { dry_run } => cmd_sync(&ctx, dry_run),
        Command::Auth { action } => match action {
            AuthAction::Login { device } => cmd_login(&ctx, device),
            AuthAction::Status => cmd_status(&ctx),
            AuthAction::Logout => cmd_logout(&ctx),
        },
        Command::Init { force } => cmd_init(&ctx, force),
        Command::Check => cmd_check(&ctx),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            log::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_sync(ctx: &Ctx, dry_run: bool) -> Result<ExitCode> {
    let cfg = ctx.config()?;
    log::info!(
        "canvas-todo-sync v{}{}",
        env!("CARGO_PKG_VERSION"),
        if dry_run { "（dry-run）" } else { "" }
    );

    let canvas = CanvasClient::new(cfg.canvas_base_url(), &cfg.canvas_token()?);
    canvas.profile().context("Canvas 认证失败")?;
    log::info!("Canvas 认证成功");

    let todo = microsoft::connect(
        &ctx.token_store(&cfg),
        &cfg.microsoft.client_id,
        &cfg.microsoft.tenant,
    )
    .context("Microsoft 认证失败")?;
    log::info!("Microsoft 认证成功");

    let mut rec = Reconciler::new(&todo, dry_run)?;
    let mut source = CanvasSync::new(&cfg, &canvas)?;
    source.run(&mut rec)?;

    let s = &rec.stats;
    log::info!(
        "完成！已处理 {} 门课程中的 {} 个项目：新建 {}，更新 {}，失败 {}{}",
        source.courses,
        s.items,
        s.created,
        s.updated,
        s.failed,
        if s.duplicates > 0 {
            format!("，重复 {}", s.duplicates)
        } else {
            String::new()
        }
    );
    if source.errors > 0 && !cfg.sync.ignore_errors {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_login(ctx: &Ctx, device: bool) -> Result<ExitCode> {
    let cfg = ctx.config_or_default()?;
    let store = ctx.token_store(&cfg);
    let oauth = auth::OAuth::new(&cfg.microsoft.client_id, &cfg.microsoft.tenant);
    let token = if device {
        oauth.login_with_device_code()?
    } else {
        oauth.login_with_browser()?
    };
    let refresh = token
        .refresh_token
        .context("响应中没有 refresh_token，请确认授权范围包含 offline_access")?;
    store.save(&auth::StoredToken {
        refresh_token: refresh,
        client_id: oauth.client_id.clone(),
        tenant: oauth.tenant.clone(),
        updated_at: Timestamp::now(),
    })?;
    let me = microsoft::GraphTodo::new(&token.access_token).me()?;
    log::info!(
        "登录成功：{}（{}），令牌已保存到 {}",
        me.display_name.unwrap_or_default(),
        me.user_principal_name.or(me.mail).unwrap_or_default(),
        store.path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_status(ctx: &Ctx) -> Result<ExitCode> {
    let cfg = ctx.config_or_default()?;
    let store = ctx.token_store(&cfg);
    let Some(saved) = store.load()? else {
        log::warn!("未登录（{} 不存在）", store.path.display());
        return Ok(ExitCode::FAILURE);
    };
    log::info!(
        "令牌文件：{}（client_id={}，tenant={}，更新于 {}）",
        store.path.display(),
        saved.client_id,
        saved.tenant,
        saved.updated_at
    );
    let todo = microsoft::connect(&store, &cfg.microsoft.client_id, &cfg.microsoft.tenant)?;
    let me = todo.me()?;
    log::info!(
        "已登录：{}（{}）",
        me.display_name.unwrap_or_default(),
        me.user_principal_name.or(me.mail).unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_logout(ctx: &Ctx) -> Result<ExitCode> {
    let cfg = ctx.config_or_default()?;
    let store = ctx.token_store(&cfg);
    if store.delete()? {
        log::info!("已删除 {}", store.path.display());
    } else {
        log::info!("未登录，无需操作");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(ctx: &Ctx, force: bool) -> Result<ExitCode> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = &ctx.config_path;
    if path.exists() && !force {
        bail!("{} 已存在（使用 --force 覆盖）", path.display());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(config::EXAMPLE.as_bytes()))
        .with_context(|| format!("无法写入 {}", path.display()))?;
    log::info!("已生成配置文件 {}", path.display());
    log::info!("下一步：填写 Canvas 令牌，然后运行 `canvas-todo-sync auth login`");
    Ok(ExitCode::SUCCESS)
}

fn cmd_check(ctx: &Ctx) -> Result<ExitCode> {
    let cfg = ctx.config()?;
    log::info!("配置文件正确：{}", ctx.config_path.display());

    let mut ok = true;
    let canvas = cfg
        .canvas_token()
        .map(|t| CanvasClient::new(cfg.canvas_base_url(), &t));
    match canvas.and_then(|c| c.profile()) {
        Ok(p) => log::info!(
            "Canvas 认证成功：{}（用户 ID {}）",
            p.name.unwrap_or_default(),
            p.id
        ),
        Err(e) => {
            ok = false;
            log::error!("Canvas 认证失败：{e:#}");
        }
    }

    let store = ctx.token_store(&cfg);
    let res = microsoft::connect(&store, &cfg.microsoft.client_id, &cfg.microsoft.tenant)
        .and_then(|t| Ok((t.me()?, t.lists()?)));
    match res {
        Ok((me, lists)) => log::info!(
            "Microsoft 认证成功：{}（{}），共 {} 个 To Do 列表",
            me.display_name.unwrap_or_default(),
            me.user_principal_name.or(me.mail).unwrap_or_default(),
            lists.len()
        ),
        Err(e) => {
            ok = false;
            log::error!("Microsoft 认证失败：{e:#}");
        }
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
