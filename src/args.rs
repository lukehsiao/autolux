use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};

const AFTER_LONG_HELP: &str = "\
autolux finds the ambient light sensor and the panel on its own, and writes
through systemd-logind, so it needs no root. Run it inside the graphical
session, typically as a systemd user service.

Examples:
  Never go below 2% or above 75%:
      autolux --min 2 --max 75

  The same, from a systemd EnvironmentFile:
      AUTOLUX_MIN=2
      AUTOLUX_MAX=75
";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, after_long_help = AFTER_LONG_HELP)]
pub struct Args {
    /// Brightness in a dark room, as a percentage of the panel's maximum
    #[arg(long, env = "AUTOLUX_MIN", value_name = "PERCENT", default_value_t = 5)]
    pub min: u8,
    /// Brightness in a bright room, as a percentage of the panel's maximum
    #[arg(
        long,
        env = "AUTOLUX_MAX",
        value_name = "PERCENT",
        default_value_t = 100
    )]
    pub max: u8,
    // InfoLevel: a daemon's journal is how you check what it did, and fades
    // log one line each, so info is useful rather than noisy.
    #[command(flatten)]
    pub verbose: Verbosity<InfoLevel>,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Args;

    #[test]
    fn verify_app() {
        Args::command().debug_assert();
    }
}
