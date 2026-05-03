use chat_cli::App;
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    let mut app: App = App::try_init().await?;

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
