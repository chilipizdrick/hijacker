use clap::Parser;
use hijacker::args::CliArgs;

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = CliArgs::parse();
    hijacker::run(args)
}
