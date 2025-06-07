mod time_command;
mod thread_tracker;
mod command_tree;

use clap::{command, Parser, Subcommand};


#[derive(Subcommand)]
enum DagzCommands {
    Time(time_command::TimeCommand),
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: DagzCommands,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        DagzCommands::Time(time_cmd) => time_cmd.run(),
    }
}
