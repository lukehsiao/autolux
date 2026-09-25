use std::io::{self, IsTerminal};

use clap::Parser;
use miette::Result;
use tracing_log::AsTrace;

use autolux::args::Args;

fn main() -> Result<()> {
    let args = Args::parse();

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(format!(
            "autolux={}",
            args.verbose.log_level_filter().as_trace()
        ))
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal());
    // systemd sets JOURNAL_STREAM when stderr goes to the journal, which
    // timestamps every line itself.
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        subscriber.without_time().init();
    } else {
        subscriber.init();
    }

    // `?` converts `autolux::error::Error` into a `miette::Report` via its
    // `Diagnostic` impl, so codes and help render.
    autolux::run(&args)?;
    Ok(())
}
