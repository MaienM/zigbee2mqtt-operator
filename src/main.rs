use argh::FromArgs;
use zigbee2mqtt_operator::commands;

/// Zigbee2MQTT operator management.
#[derive(FromArgs, Debug)]
struct TopLevel {
    #[argh(subcommand)]
    command: Command,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
enum Command {
    Run(commands::run::Args),
    Crds(commands::crdgen::Args),
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let args: TopLevel = argh::from_env();
    match args.command {
        Command::Run(args) => commands::run::main(args).await,
        Command::Crds(args) => commands::crdgen::main(args),
    }
}
