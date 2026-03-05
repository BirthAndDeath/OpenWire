use chat_cli::App;
use chat_cli::notui::*;
use chat_cli::tui::*;
use clap::Parser;
use std::io::IsTerminal;
#[derive(Parser)]
#[command(version="0.1.0", author="Wang yuxuan", about="a chat cli app", long_about = None)]
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

    println!(
        "args: use_json={:?},no_tui={:?}\n",
        args.use_json, args.no_tui
    );
    println!("Hello world!\n ");
    let mut app: App = App::try_init().await.unwrap();

    if std::io::stdout().is_terminal() {
        //此处是面向终端用户的输出界面，除此以外是对shell调用，可精简交互
        if args.no_tui {
            no_tui_run(&mut app).await?;
        } else {
            tui_run(&mut app).await?;
        }
    }

    Ok(())
}
