use chat_cli::App;
use chat_cli::error::CliResult;
use chat_cli::notui::no_tui_run;
use chat_cli::tui::tui_run;
use chat_cli::use_json::json_run;
use clap::Parser;

#[derive(Parser)]
#[command(version="0.1.0", author="BAD and deepseekv4", about="a chat cli app", long_about = None)]
pub struct Cli {
    ///是否使用json的格式输出
    #[arg(long)]
    use_json: bool,
    ///是否使用终端ui界面
    #[arg(long)]
    no_tui: bool,
    ///用户密码（原始密码，内部使用 Argon2id 派生为 256 位密钥）
    ///用于 Keyring 不可用时的降级加密文件存储
    #[arg(long)]
    password: Option<String>,
}

#[tokio::main]
async fn main() -> CliResult<()> {
    let args = Cli::parse();

    let mut app: App = App::try_init(args.password.as_deref()).await?;

    if args.no_tui {
        if args.use_json {
            json_run(&mut app).await?;
        } else {
            no_tui_run(&mut app).await?;
        }
    } else {
        tui_run(&mut app).await?;
    }

    Ok(())
}
