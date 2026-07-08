use clap::Parser;
use openwire_cli::App;
use openwire_cli::error::CliResult;
use openwire_cli::notui::no_tui_run;
use openwire_cli::tui::tui_run;
use openwire_cli::use_json::json_run;

#[derive(Parser)]
#[command(version="0.1.0", author="BAD and deepseekv4", about="a chat cli app", long_about = None)]
pub struct Cli {
    #[arg(long)]
    use_json: bool,
    #[arg(long)]
    no_tui: bool,
}

#[tokio::main]
async fn main() -> CliResult<()> {
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